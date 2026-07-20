"""Report deprecated dependencies and runtime support without failing on findings."""

from __future__ import annotations

import calendar
import html
import json
import os
import re
import subprocess
import sys
import tomllib
import urllib.parse
import urllib.request
from dataclasses import asdict, dataclass
from datetime import date, datetime, timezone
from pathlib import Path
from typing import Any


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
REPORT_DIRECTORY = REPOSITORY_ROOT / "target" / "ci-reports" / "deprecations"
USER_AGENT = "dicom-anon-dependency-health/0.1"
WARNING_WINDOW_DAYS = 180


@dataclass(frozen=True)
class Finding:
    surface: str
    dependency: str
    version: str
    detail: str


def fetch(url: str) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=30) as response:
        return response.read()


def fetch_json(url: str) -> Any:
    return json.loads(fetch(url))


def write_json(path: Path, value: Any) -> None:
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def strip_html(value: str) -> str:
    return html.unescape(re.sub(r"<[^>]+>", "", value)).strip()


def exact_version(requirement: str) -> str | None:
    match = re.fullmatch(r"=?=?\s*([0-9][^\s,;]*)", requirement)
    return match.group(1) if match else None


def command_version(command: list[str]) -> tuple[str, str | None]:
    try:
        completed = subprocess.run(
            command,
            cwd=REPOSITORY_ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError as error:
        return f"unavailable: {error}", f"version command could not start: {command[0]}"
    output = completed.stdout.strip() or completed.stderr.strip()
    version = output.splitlines()[0] if output else f"exit {completed.returncode}"
    if completed.returncode != 0 or not output:
        return version, f"version command failed: {' '.join(command)}"
    return version, None


def collect_tool_versions() -> tuple[dict[str, str], list[str]]:
    commands = {
        "rustc": ["rustc", "--version"],
        "cargo": ["cargo", "--version"],
        "python": [sys.executable, "--version"],
        "node": ["node", "--version"],
        "npm": ["npm", "--version"],
        "uv": ["uv", "--version"],
    }
    versions: dict[str, str] = {}
    errors: list[str] = []
    for name, command in commands.items():
        version, error = command_version(command)
        versions[name] = version
        if error is not None:
            errors.append(f"{name} {error}")
    return versions, errors


def build_manifest(
    findings: list[Finding],
    errors: list[str],
    tool_versions: dict[str, str],
    *,
    checked_at: str | None = None,
) -> dict[str, Any]:
    return {
        "checked_at": checked_at or datetime.now(timezone.utc).isoformat(),
        "finding_policy": "informational",
        "incomplete_check_policy": "failure",
        "sources": [
            "crates.io API",
            "npm registry metadata",
            "PyPI metadata",
            "Node.js Release schedule",
            "Python Developer Guide versions table",
            "Cargo future-incompatibility report",
        ],
        "audited_inputs": [
            {
                "manifests": ["Cargo.toml"],
                "lockfiles": ["Cargo.lock"],
                "groups": [
                    "workspace.dependencies",
                    "workspace --all-targets --all-features",
                ],
            },
            {
                "manifests": ["bindings/node/package.json"],
                "lockfiles": ["bindings/node/package-lock.json"],
                "groups": ["dependencies", "devDependencies", "engines.node"],
            },
            {
                "manifests": ["tools/ci/pyproject.toml"],
                "lockfiles": ["tools/ci/uv.lock"],
                "groups": ["project.dependencies"],
            },
            {
                "manifests": ["bindings/python/pyproject.toml"],
                "lockfiles": [],
                "groups": ["project.requires-python"],
            },
        ],
        "tool_versions": tool_versions,
        "findings": [asdict(finding) for finding in findings],
        "errors": errors,
    }


def check_rust_dependencies() -> tuple[list[Finding], list[str]]:
    manifest = tomllib.loads(
        (REPOSITORY_ROOT / "Cargo.toml").read_text(encoding="utf-8")
    )
    findings: list[Finding] = []
    errors: list[str] = []
    report: list[dict[str, Any]] = []
    for name, specification in sorted(manifest["workspace"]["dependencies"].items()):
        requirement = (
            specification
            if isinstance(specification, str)
            else specification.get("version")
        )
        version = exact_version(requirement) if isinstance(requirement, str) else None
        if version is None:
            continue
        try:
            metadata = fetch_json(f"https://crates.io/api/v1/crates/{name}/{version}")
            yanked = bool(metadata["version"]["yanked"])
            report.append({"name": name, "version": version, "yanked": yanked})
            if yanked:
                findings.append(
                    Finding("Rust", name, version, "direct version is yanked")
                )
        except (OSError, KeyError, TypeError, ValueError) as error:
            errors.append(f"Rust metadata for {name} {version}: {error}")
    write_json(REPORT_DIRECTORY / "rust-direct.json", report)
    return findings, errors


def check_node_dependencies() -> tuple[list[Finding], list[str]]:
    lock = json.loads(
        (REPOSITORY_ROOT / "bindings" / "node" / "package-lock.json").read_text(
            encoding="utf-8"
        )
    )
    root = lock["packages"][""]
    direct_names = sorted(
        set(root.get("dependencies", {})) | set(root.get("devDependencies", {}))
    )
    findings: list[Finding] = []
    errors: list[str] = []
    report: list[dict[str, Any]] = []
    for name in direct_names:
        version = lock["packages"][f"node_modules/{name}"]["version"]
        encoded_name = urllib.parse.quote(name, safe="")
        try:
            metadata = fetch_json(
                f"https://registry.npmjs.org/{encoded_name}/{version}"
            )
            deprecated = metadata.get("deprecated")
            report.append({"name": name, "version": version, "deprecated": deprecated})
            if deprecated:
                findings.append(Finding("Node", name, version, str(deprecated)))
        except (OSError, KeyError, TypeError, ValueError) as error:
            errors.append(f"npm metadata for {name} {version}: {error}")
    write_json(REPORT_DIRECTORY / "node-direct.json", report)
    return findings, errors


def check_python_dependencies() -> tuple[list[Finding], list[str]]:
    manifest = tomllib.loads(
        (REPOSITORY_ROOT / "tools" / "ci" / "pyproject.toml").read_text(
            encoding="utf-8"
        )
    )
    findings: list[Finding] = []
    errors: list[str] = []
    report: list[dict[str, Any]] = []
    for requirement in manifest["project"]["dependencies"]:
        match = re.fullmatch(r"([A-Za-z0-9_.-]+)==([^;\s]+)", requirement)
        if match is None:
            errors.append(f"CI dependency is not exactly pinned: {requirement}")
            continue
        name, version = match.groups()
        try:
            metadata = fetch_json(f"https://pypi.org/pypi/{name}/{version}/json")
            urls = metadata.get("urls", [])
            yanked = bool(urls) and all(bool(item.get("yanked")) for item in urls)
            reason = next(
                (
                    str(item.get("yanked_reason"))
                    for item in urls
                    if item.get("yanked_reason")
                ),
                "direct version is yanked",
            )
            report.append({"name": name, "version": version, "yanked": yanked})
            if yanked:
                findings.append(Finding("Python CI", name, version, reason))
        except (OSError, KeyError, TypeError, ValueError) as error:
            errors.append(f"PyPI metadata for {name} {version}: {error}")
    write_json(REPORT_DIRECTORY / "python-direct.json", report)
    return findings, errors


def parse_year_month(value: str) -> date:
    year, month = (int(part) for part in value.split("-", maxsplit=1))
    return date(year, month, calendar.monthrange(year, month)[1])


def check_runtime_support() -> tuple[list[Finding], list[str]]:
    findings: list[Finding] = []
    errors: list[str] = []
    report: dict[str, Any] = {}
    today = datetime.now(timezone.utc).date()

    try:
        package = json.loads(
            (REPOSITORY_ROOT / "bindings" / "node" / "package.json").read_text(
                encoding="utf-8"
            )
        )
        engine = package["engines"]["node"]
        match = re.fullmatch(r">=(\d+)", engine)
        if match is None:
            raise ValueError(f"unsupported Node engine expression: {engine}")
        major = match.group(1)
        schedule = fetch_json(
            "https://raw.githubusercontent.com/nodejs/Release/main/schedule.json"
        )
        end = date.fromisoformat(schedule[f"v{major}"]["end"])
        days_remaining = (end - today).days
        report["node"] = {
            "claim": engine,
            "major": major,
            "end_of_life": end.isoformat(),
            "days_remaining": days_remaining,
            "source": "https://github.com/nodejs/Release/blob/main/schedule.json",
        }
        if days_remaining < 0:
            findings.append(
                Finding("Runtime", "Node.js", major, "support claim is EOL")
            )
        elif days_remaining <= WARNING_WINDOW_DAYS:
            findings.append(
                Finding("Runtime", "Node.js", major, f"EOL in {days_remaining} days")
            )
    except (OSError, KeyError, TypeError, ValueError) as error:
        errors.append(f"Node.js support schedule: {error}")

    try:
        python_manifest = tomllib.loads(
            (REPOSITORY_ROOT / "bindings" / "python" / "pyproject.toml").read_text(
                encoding="utf-8"
            )
        )
        requirement = python_manifest["project"]["requires-python"]
        match = re.fullmatch(r">=(\d+\.\d+)", requirement)
        if match is None:
            raise ValueError(f"unsupported Python requirement: {requirement}")
        minimum = match.group(1)
        page = fetch("https://devguide.python.org/versions/").decode("utf-8")
        row_match = re.search(
            rf"<tr[^>]*>\s*<td><p>{re.escape(minimum)}</p></td>(.*?)</tr>",
            page,
            re.DOTALL,
        )
        if row_match is None:
            raise ValueError(f"Python {minimum} row not found")
        cells = [
            strip_html(cell)
            for cell in re.findall(
                r"<td><p>(.*?)</p></td>", row_match.group(0), re.DOTALL
            )
        ]
        if len(cells) < 5:
            raise ValueError(f"Python {minimum} lifecycle row is incomplete")
        status = cells[2]
        end = parse_year_month(cells[4])
        days_remaining = (end - today).days
        report["python"] = {
            "claim": requirement,
            "minimum": minimum,
            "status": status,
            "end_of_life": end.isoformat(),
            "days_remaining": days_remaining,
            "source": "https://devguide.python.org/versions/",
        }
        if status == "end-of-life" or days_remaining < 0:
            findings.append(
                Finding("Runtime", "Python", minimum, "support claim is EOL")
            )
        elif days_remaining <= WARNING_WINDOW_DAYS:
            findings.append(
                Finding("Runtime", "Python", minimum, f"EOL in {days_remaining} days")
            )
    except (OSError, KeyError, TypeError, UnicodeError, ValueError) as error:
        errors.append(f"Python support schedule: {error}")

    write_json(REPORT_DIRECTORY / "runtime-support.json", report)
    return findings, errors


def check_rust_future_incompatibility() -> tuple[list[Finding], list[str]]:
    command = [
        "cargo",
        "check",
        "--locked",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--future-incompat-report",
    ]
    try:
        completed = subprocess.run(
            command,
            cwd=REPOSITORY_ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError as error:
        write_json(
            REPORT_DIRECTORY / "rust-future-incompatibility.json",
            {"command": command, "error": str(error), "status": "incomplete"},
        )
        return [], [f"Rust future-incompatibility check could not start: {error}"]
    output = f"{completed.stdout}\n{completed.stderr}"
    write_json(
        REPORT_DIRECTORY / "rust-future-incompatibility.json",
        {
            "command": command,
            "exit_code": completed.returncode,
            "stdout": completed.stdout,
            "stderr": completed.stderr,
        },
    )
    if completed.returncode != 0:
        return [], [f"Rust future-incompatibility check exited {completed.returncode}"]
    if "will be rejected by a future version of Rust" in output:
        return [
            Finding("Rust", "workspace", "locked", "future-incompatible code reported")
        ], []
    return [], []


def write_summary(findings: list[Finding], errors: list[str]) -> None:
    lines = [
        "# Dependency deprecation report",
        "",
        f"Checked: {datetime.now(timezone.utc).isoformat()}",
        "",
    ]
    if findings:
        lines.extend(
            [
                "| Surface | Dependency | Version | Finding |",
                "|---|---|---|---|",
                *[
                    f"| {item.surface} | `{item.dependency}` | `{item.version}` | {item.detail} |"
                    for item in findings
                ],
            ]
        )
    else:
        lines.append(
            "No deprecated, yanked, unsupported, or future-incompatible claims found."
        )
    if errors:
        lines.extend(
            ["", "## Incomplete checks", "", *[f"- {error}" for error in errors]]
        )
    lines.extend(["", "Findings are informational. Incomplete checks fail this job."])
    summary = "\n".join(lines) + "\n"
    (REPORT_DIRECTORY / "summary.md").write_text(summary, encoding="utf-8")
    if step_summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(step_summary).open("a", encoding="utf-8") as summary_file:
            summary_file.write(summary)


def main() -> int:
    REPORT_DIRECTORY.mkdir(parents=True, exist_ok=True)
    findings: list[Finding] = []
    tool_versions, errors = collect_tool_versions()
    for check in (
        check_rust_future_incompatibility,
        check_rust_dependencies,
        check_node_dependencies,
        check_python_dependencies,
        check_runtime_support,
    ):
        check_findings, check_errors = check()
        findings.extend(check_findings)
        errors.extend(check_errors)
    write_json(
        REPORT_DIRECTORY / "manifest.json",
        build_manifest(findings, errors, tool_versions),
    )
    write_summary(findings, errors)
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
