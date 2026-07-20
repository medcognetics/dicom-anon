"""Run every dependency security scanner and enforce the aggregate result."""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
REPORT_DIRECTORY = REPOSITORY_ROOT / "target" / "ci-reports" / "security"
ACTION_REFERENCE = re.compile(r"^\s*uses:\s*([^\s@]+)@([^\s#]+)", re.MULTILINE)
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")


@dataclass(frozen=True)
class ScanResult:
    name: str
    command: list[str]
    working_directory: str
    exit_code: int | None
    status: str
    output_file: str
    error: str | None = None


def run_command(name: str, command: list[str], working_directory: Path) -> ScanResult:
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
    status = classify_result(completed.returncode, parsed_output, parse_error)
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
    )


def parse_json_output(output: str) -> tuple[Any, str | None]:
    if not output.strip():
        return None, "scanner produced no JSON output"
    try:
        return json.loads(output), None
    except json.JSONDecodeError as error:
        return None, f"invalid JSON output: {error}"


def classify_result(exit_code: int, parsed_output: Any, parse_error: str | None) -> str:
    if parse_error is not None:
        return "incomplete"
    if isinstance(parsed_output, dict) and "error" in parsed_output:
        return "incomplete"
    if exit_code == 0:
        return "clean"
    if parsed_output is not None:
        return "findings"
    return "incomplete"


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


def write_summary(results: list[ScanResult]) -> None:
    lines = [
        "# Dependency security audit",
        "",
        f"Checked: {datetime.now(timezone.utc).isoformat()}",
        "",
        "| Scanner | Status | Exit | Report |",
        "|---|---|---:|---|",
    ]
    for result in results:
        exit_code = "-" if result.exit_code is None else str(result.exit_code)
        lines.append(
            f"| `{result.name}` | {result.status} | {exit_code} | `{result.output_file}` |"
        )
    lines.extend(
        [
            "",
            "All scanners ran before enforcement. Findings and incomplete scans both fail policy.",
        ]
    )
    summary = "\n".join(lines) + "\n"
    (REPORT_DIRECTORY / "summary.md").write_text(summary, encoding="utf-8")
    if step_summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(step_summary).open("a", encoding="utf-8") as summary_file:
            summary_file.write(summary)


def main() -> int:
    REPORT_DIRECTORY.mkdir(parents=True, exist_ok=True)
    results = [
        run_command(
            "cargo-audit",
            ["cargo", "audit", "--json", "--deny", "warnings"],
            REPOSITORY_ROOT,
        ),
        run_command(
            "npm-audit",
            ["npm", "audit", "--package-lock-only", "--json"],
            REPOSITORY_ROOT / "bindings" / "node",
        ),
        scan_python_lock(),
        run_command(
            "zizmor",
            ["zizmor", "--format", "json", "."],
            REPOSITORY_ROOT,
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
    return 1 if any(result.status != "clean" for result in results) else 0


if __name__ == "__main__":
    raise SystemExit(main())
