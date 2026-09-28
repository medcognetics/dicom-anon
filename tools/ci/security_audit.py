"""Run every dependency security scanner and enforce the aggregate result."""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from collections.abc import Callable, Iterable
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, TypeGuard


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
REPORT_DIRECTORY = REPOSITORY_ROOT / "target" / "ci-reports" / "security"
ACTION_REFERENCE = re.compile(r"^\s*uses:\s*([^\s@]+)@([^\s#]+)", re.MULTILINE)
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")
NON_BLOCKING_STATUSES = frozenset({"clean", "warnings"})
DESCRIBED_STATUSES = frozenset({"findings", "warnings"})
MAX_DETAILS_PER_SCANNER = 25
MAX_DETAIL_LENGTH = 300
ZIZMOR_CONFIDENCE_LEVELS = frozenset({"Low", "Medium", "High"})
ENFORCEMENT_POLICY = {
    "non_blocking_statuses": ["clean", "warnings"],
    "blocking_statuses": ["findings", "incomplete"],
    "cargo-audit": {
        "warnings": "unmaintained-only advisories",
        "findings": "vulnerabilities or other warning classes",
    },
    "zizmor": {
        "warnings": "Low-confidence findings",
        "findings": "Medium- or High-confidence findings",
    },
    "default_scanners": {
        "findings": "npm, pip, action-pin, or other scanner findings",
    },
    "all_scanners": {
        "incomplete": "scanner, database, network, parse, or schema errors",
    },
}
ResultClassifier = Callable[[int, Any, str | None], str]


@dataclass(frozen=True)
class ScanResult:
    name: str
    command: list[str]
    working_directory: str
    exit_code: int | None
    status: str
    output_file: str
    error: str | None = None
    details: tuple[str, ...] = ()


def run_command(
    name: str,
    command: list[str],
    working_directory: Path,
    classifier: ResultClassifier,
) -> ScanResult:
    output_file = REPORT_DIRECTORY / f"{name}.json"
    try:
        completed = subprocess.run(
            command,
            cwd=working_directory,
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError as error:
        write_json(
            output_file,
            {"command": command, "error": str(error), "status": "incomplete"},
        )
        return ScanResult(
            name,
            command,
            str(working_directory.relative_to(REPOSITORY_ROOT) or "."),
            None,
            "incomplete",
            output_file.name,
            str(error),
        )

    parsed_output, parse_error = parse_json_output(completed.stdout)
    status = classifier(completed.returncode, parsed_output, parse_error)
    details = describe_findings(name, status, parsed_output)
    write_json(
        output_file,
        {
            "command": command,
            "exit_code": completed.returncode,
            "status": status,
            "stdout": parsed_output if parse_error is None else completed.stdout,
            "stderr": completed.stderr,
            "parse_error": parse_error,
        },
    )
    return ScanResult(
        name,
        command,
        str(working_directory.relative_to(REPOSITORY_ROOT) or "."),
        completed.returncode,
        status,
        output_file.name,
        parse_error,
        details,
    )


def parse_json_output(output: str) -> tuple[Any, str | None]:
    if not output.strip():
        return None, "scanner produced no JSON output"
    try:
        return json.loads(output), None
    except json.JSONDecodeError as error:
        return None, f"invalid JSON output: {error}"


def is_nonempty_string(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def is_nonnegative_integer(value: Any) -> TypeGuard[int]:
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def has_cargo_advisory_identity(value: Any) -> bool:
    if not isinstance(value, dict):
        return False
    advisory = value.get("advisory")
    package = value.get("package")
    return (
        isinstance(advisory, dict)
        and is_nonempty_string(advisory.get("id"))
        and isinstance(package, dict)
        and is_nonempty_string(package.get("name"))
        and is_nonempty_string(package.get("version"))
    )


def classify_cargo_audit_result(
    exit_code: int, parsed_output: Any, parse_error: str | None
) -> str:
    if parse_error is not None or not isinstance(parsed_output, dict):
        return "incomplete"
    if "error" in parsed_output or exit_code not in (0, 1):
        return "incomplete"

    database = parsed_output.get("database")
    lockfile = parsed_output.get("lockfile")
    settings = parsed_output.get("settings")
    vulnerabilities = parsed_output.get("vulnerabilities")
    warnings = parsed_output.get("warnings")
    if (
        not isinstance(database, dict)
        or not is_nonnegative_integer(database.get("advisory-count"))
        or not is_nonempty_string(database.get("last-commit"))
        or not is_nonempty_string(database.get("last-updated"))
        or not isinstance(lockfile, dict)
        or not is_nonnegative_integer(lockfile.get("dependency-count"))
        or not isinstance(settings, dict)
        or not isinstance(vulnerabilities, dict)
        or not isinstance(warnings, dict)
    ):
        return "incomplete"

    vulnerability_count = vulnerabilities.get("count")
    vulnerabilities_found = vulnerabilities.get("found")
    vulnerability_list = vulnerabilities.get("list")
    if (
        not isinstance(vulnerability_count, int)
        or isinstance(vulnerability_count, bool)
        or not isinstance(vulnerabilities_found, bool)
        or not isinstance(vulnerability_list, list)
        or vulnerability_count != len(vulnerability_list)
        or vulnerabilities_found != (vulnerability_count > 0)
        or any(
            not has_cargo_advisory_identity(vulnerability)
            for vulnerability in vulnerability_list
        )
        or any(
            not is_nonempty_string(warning_kind)
            or not isinstance(warning_entries, list)
            for warning_kind, warning_entries in warnings.items()
        )
        or any(
            not has_cargo_advisory_identity(warning_entry)
            for warning_entries in warnings.values()
            for warning_entry in warning_entries
        )
    ):
        return "incomplete"

    if vulnerability_count > 0:
        return "findings"

    active_warning_kinds = {
        warning_kind
        for warning_kind, warning_entries in warnings.items()
        if warning_entries
    }
    if active_warning_kinds:
        return "warnings" if active_warning_kinds == {"unmaintained"} else "findings"
    return "clean" if exit_code == 0 else "incomplete"


def classify_npm_audit_result(
    exit_code: int, parsed_output: Any, parse_error: str | None
) -> str:
    if (
        parse_error is not None
        or exit_code not in (0, 1)
        or not isinstance(parsed_output, dict)
        or "error" in parsed_output
    ):
        return "incomplete"

    report_version = parsed_output.get("auditReportVersion")
    metadata = parsed_output.get("metadata")
    vulnerabilities = parsed_output.get("vulnerabilities")
    if (
        not is_nonnegative_integer(report_version)
        or report_version == 0
        or not isinstance(metadata, dict)
        or not isinstance(vulnerabilities, dict)
    ):
        return "incomplete"

    vulnerability_summary = metadata.get("vulnerabilities")
    if not isinstance(vulnerability_summary, dict):
        return "incomplete"
    vulnerability_total = vulnerability_summary.get("total")
    if (
        not is_nonnegative_integer(vulnerability_total)
        or any(
            not is_nonempty_string(package_name)
            or not isinstance(vulnerability, dict)
            or not is_nonempty_string(vulnerability.get("severity"))
            for package_name, vulnerability in vulnerabilities.items()
        )
        or bool(vulnerabilities) != (vulnerability_total > 0)
    ):
        return "incomplete"

    if vulnerabilities:
        return "findings"
    return "clean" if exit_code == 0 else "incomplete"


def classify_pip_audit_result(
    exit_code: int, parsed_output: Any, parse_error: str | None
) -> str:
    if (
        parse_error is not None
        or exit_code not in (0, 1)
        or not isinstance(parsed_output, dict)
        or "error" in parsed_output
    ):
        return "incomplete"

    dependencies = parsed_output.get("dependencies")
    fixes = parsed_output.get("fixes")
    if (
        not isinstance(dependencies, list)
        or not isinstance(fixes, list)
        or any(not isinstance(fix, dict) for fix in fixes)
    ):
        return "incomplete"

    has_findings = False
    for dependency in dependencies:
        if not isinstance(dependency, dict):
            return "incomplete"
        name = dependency.get("name")
        version = dependency.get("version")
        vulnerabilities = dependency.get("vulns")
        if (
            not is_nonempty_string(name)
            or not is_nonempty_string(version)
            or not isinstance(vulnerabilities, list)
            or any(
                not isinstance(vulnerability, dict)
                or not is_nonempty_string(vulnerability.get("id"))
                for vulnerability in vulnerabilities
            )
        ):
            return "incomplete"
        has_findings |= bool(vulnerabilities)

    if has_findings:
        return "findings"
    return "clean" if exit_code == 0 else "incomplete"


def classify_zizmor_result(
    exit_code: int, parsed_output: Any, parse_error: str | None
) -> str:
    if parse_error is not None or exit_code != 0 or not isinstance(parsed_output, list):
        return "incomplete"
    if not parsed_output:
        return "clean"

    has_blocking_confidence = False
    for finding in parsed_output:
        if not isinstance(finding, dict):
            return "incomplete"
        identifier = finding.get("ident")
        determinations = finding.get("determinations")
        locations = finding.get("locations")
        if (
            not is_nonempty_string(identifier)
            or not isinstance(determinations, dict)
            or not isinstance(locations, list)
            or not locations
            or any(not isinstance(location, dict) for location in locations)
        ):
            return "incomplete"
        confidence = determinations.get("confidence")
        severity = determinations.get("severity")
        if confidence not in ZIZMOR_CONFIDENCE_LEVELS or not is_nonempty_string(
            severity
        ):
            return "incomplete"
        has_blocking_confidence |= confidence in {"Medium", "High"}

    return "findings" if has_blocking_confidence else "warnings"


def nested_value(value: Any, *keys: str) -> Any:
    for key in keys:
        if not isinstance(value, dict):
            return None
        value = value.get(key)
    return value


def describe_cargo_advisory(entry: dict[str, Any]) -> str:
    package = entry["package"]
    return f"{entry['advisory']['id']} `{package['name']}` {package['version']}"


def describe_cargo_audit(output: Any) -> list[str]:
    vulnerabilities = [
        f"vulnerability {describe_cargo_advisory(entry)}"
        for entry in output["vulnerabilities"]["list"]
    ]
    warnings = [
        f"{warning_kind} {describe_cargo_advisory(entry)}"
        for warning_kind, entries in sorted(output["warnings"].items())
        for entry in entries
    ]
    return vulnerabilities + warnings


def describe_npm_audit(output: Any) -> list[str]:
    return [
        f"`{package_name}` ({vulnerability['severity']})"
        for package_name, vulnerability in sorted(output["vulnerabilities"].items())
    ]


def describe_pip_audit(output: Any) -> list[str]:
    return [
        f"`{dependency['name']}` {dependency['version']}: "
        + ", ".join(
            dict.fromkeys(vulnerability["id"] for vulnerability in dependency["vulns"])
        )
        for dependency in output["dependencies"]
        if dependency["vulns"]
    ]


def describe_zizmor_location(locations: list[dict[str, Any]]) -> str:
    location = next(
        (
            candidate
            for candidate in locations
            if nested_value(candidate, "symbolic", "kind") == "Primary"
        ),
        locations[0],
    )
    path = nested_value(location, "symbolic", "key", "Local", "verbatim_path")
    if not is_nonempty_string(path):
        return ""
    row = nested_value(location, "concrete", "location", "start_point", "row")
    return f" at {path}:{row + 1}" if is_nonnegative_integer(row) else f" at {path}"


def describe_zizmor(output: Any) -> list[str]:
    return [
        f"{finding['ident']} ({finding['determinations']['confidence']} confidence, "
        f"{finding['determinations']['severity']} severity)"
        + describe_zizmor_location(finding["locations"])
        for finding in output
    ]


FINDING_DESCRIBERS: dict[str, Callable[[Any], list[str]]] = {
    "cargo-audit": describe_cargo_audit,
    "npm-audit": describe_npm_audit,
    "pip-audit": describe_pip_audit,
    "zizmor": describe_zizmor,
}


def describe_findings(name: str, status: str, parsed_output: Any) -> tuple[str, ...]:
    """Name the findings of a validated result without copying raw scanner output."""
    if status == "incomplete" and isinstance(parsed_output, dict):
        if "error" in parsed_output:
            return (
                f"scanner error: {json.dumps(parsed_output['error'], sort_keys=True)}",
            )
        return ()
    describer = FINDING_DESCRIBERS.get(name)
    if describer is None or status not in DESCRIBED_STATUSES:
        return ()
    return tuple(describer(parsed_output))


def policy_passes(statuses: Iterable[str]) -> bool:
    return all(status in NON_BLOCKING_STATUSES for status in statuses)


def scan_action_pins() -> ScanResult:
    output_file = REPORT_DIRECTORY / "action-pins.json"
    findings: list[dict[str, str]] = []
    workflow_directory = REPOSITORY_ROOT / ".github" / "workflows"
    for workflow_path in sorted(workflow_directory.glob("*.yml")):
        workflow = workflow_path.read_text(encoding="utf-8")
        for action, reference in ACTION_REFERENCE.findall(workflow):
            if not FULL_SHA.fullmatch(reference):
                findings.append(
                    {
                        "workflow": str(workflow_path.relative_to(REPOSITORY_ROOT)),
                        "action": action,
                        "reference": reference,
                    }
                )
    status = "findings" if findings else "clean"
    write_json(
        output_file,
        {
            "command": ["internal", "validate-action-pins"],
            "status": status,
            "findings": findings,
        },
    )
    return ScanResult(
        "action-pins",
        ["internal", "validate-action-pins"],
        ".",
        1 if findings else 0,
        status,
        output_file.name,
        details=tuple(
            f"`{finding['workflow']}`: {finding['action']}@{finding['reference']}"
            for finding in findings
        ),
    )


def scan_python_lock() -> ScanResult:
    requirements_path = REPORT_DIRECTORY / "ci-requirements.txt"
    export_command = [
        "uv",
        "export",
        "--quiet",
        "--project",
        "tools/ci",
        "--frozen",
        "--no-emit-project",
        "--format",
        "requirements-txt",
        "--output-file",
        str(requirements_path),
    ]
    try:
        completed = subprocess.run(
            export_command,
            cwd=REPOSITORY_ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError as error:
        output_file = REPORT_DIRECTORY / "pip-audit.json"
        write_json(
            output_file,
            {"command": export_command, "error": str(error), "status": "incomplete"},
        )
        return ScanResult(
            "pip-audit",
            export_command,
            ".",
            None,
            "incomplete",
            output_file.name,
            str(error),
        )
    if completed.returncode != 0:
        output_file = REPORT_DIRECTORY / "pip-audit.json"
        error = f"uv export exited {completed.returncode}: {completed.stderr.strip()}"
        write_json(
            output_file,
            {
                "command": export_command,
                "exit_code": completed.returncode,
                "status": "incomplete",
                "stdout": completed.stdout,
                "stderr": completed.stderr,
            },
        )
        return ScanResult(
            "pip-audit",
            export_command,
            ".",
            completed.returncode,
            "incomplete",
            output_file.name,
            error,
        )
    return run_command(
        "pip-audit",
        [
            sys.executable,
            "-m",
            "pip_audit",
            "--requirement",
            str(requirements_path),
            "--format",
            "json",
        ],
        REPOSITORY_ROOT,
        classify_pip_audit_result,
    )


def command_version(command: list[str]) -> str:
    try:
        completed = subprocess.run(
            command,
            cwd=REPOSITORY_ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError as error:
        return f"unavailable: {error}"
    output = completed.stdout.strip() or completed.stderr.strip()
    return output.splitlines()[0] if output else f"exit {completed.returncode}"


def write_json(path: Path, value: Any) -> None:
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def truncate_detail(detail: str) -> str:
    if len(detail) <= MAX_DETAIL_LENGTH:
        return detail
    return detail[: MAX_DETAIL_LENGTH - 3] + "..."


def summary_details(results: list[ScanResult]) -> list[str]:
    lines: list[str] = []
    for result in results:
        entries = list(result.details)
        if result.error is not None:
            entries.insert(0, f"error: {result.error}")
        lines.extend(
            f"- `{result.name}`: {truncate_detail(entry)}"
            for entry in entries[:MAX_DETAILS_PER_SCANNER]
        )
        omitted = len(entries) - MAX_DETAILS_PER_SCANNER
        if omitted > 0:
            lines.append(f"- `{result.name}`: {omitted} more")
    return lines


def write_summary(results: list[ScanResult]) -> None:
    lines = [
        "# Dependency security audit",
        "",
        f"Checked: {datetime.now(timezone.utc).isoformat()}",
        "",
        "| Scanner | Status | Exit |",
        "|---|---|---:|",
    ]
    for result in results:
        exit_code = "-" if result.exit_code is None else str(result.exit_code)
        lines.append(f"| `{result.name}` | {result.status} | {exit_code} |")
    if details := summary_details(results):
        lines.extend(["", "## Findings and errors", "", *details])
    lines.extend(
        [
            "",
            "All scanners ran before enforcement. Warning-only results remain visible; "
            "findings and incomplete scans fail policy.",
            "Local runs write complete scanner output to `target/ci-reports/security/`.",
        ]
    )
    summary = "\n".join(lines) + "\n"
    (REPORT_DIRECTORY / "summary.md").write_text(summary, encoding="utf-8")
    sys.stdout.write(summary)
    if step_summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(step_summary).open("a", encoding="utf-8") as summary_file:
            summary_file.write(summary)


def main() -> int:
    REPORT_DIRECTORY.mkdir(parents=True, exist_ok=True)
    results = [
        run_command(
            "cargo-audit",
            ["cargo", "audit", "--json"],
            REPOSITORY_ROOT,
            classify_cargo_audit_result,
        ),
        run_command(
            "npm-audit",
            ["npm", "audit", "--package-lock-only", "--json"],
            REPOSITORY_ROOT / "bindings" / "node",
            classify_npm_audit_result,
        ),
        scan_python_lock(),
        run_command(
            "zizmor",
            ["zizmor", "--format", "json", "--no-exit-codes", "."],
            REPOSITORY_ROOT,
            classify_zizmor_result,
        ),
        scan_action_pins(),
    ]
    write_json(
        REPORT_DIRECTORY / "manifest.json",
        {
            "checked_at": datetime.now(timezone.utc).isoformat(),
            "advisory_sources": {
                "cargo-audit": "RustSec advisory database fetched by cargo-audit",
                "npm-audit": "npm registry advisory service",
                "pip-audit": "Python Packaging Advisory Database via pip-audit",
                "zizmor": "zizmor GitHub Actions security rules",
            },
            "audited_inputs": [
                "Cargo.lock (runtime, build, development, and test dependencies)",
                "bindings/node/package-lock.json (development and build dependencies)",
                "tools/ci/uv.lock (CI and test tools)",
                ".github/workflows/*.yml",
            ],
            "enforcement_policy": ENFORCEMENT_POLICY,
            "tool_versions": {
                "cargo-audit": command_version(["cargo", "audit", "--version"]),
                "npm": command_version(["npm", "--version"]),
                "pip-audit": command_version(
                    [sys.executable, "-m", "pip_audit", "--version"]
                ),
                "uv": command_version(["uv", "--version"]),
                "zizmor": command_version(["zizmor", "--version"]),
            },
            "results": [asdict(result) for result in results],
        },
    )
    write_summary(results)
    return 0 if policy_passes(result.status for result in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
