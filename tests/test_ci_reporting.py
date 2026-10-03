"""Unit tests for CI report classification and lifecycle parsing."""

import json
import subprocess
from datetime import date
from pathlib import Path

import pytest

from tools.ci import deprecation_report, security_audit
from tools.ci.deprecation_report import (
    Finding,
    build_manifest,
    classify_rust_future_incompatibility,
    exact_version,
    parse_python_lifecycle_date,
)


CARGO_AUDIT_METADATA = {
    "database": {
        "advisory-count": 1,
        "last-commit": "a" * 40,
        "last-updated": "2026-08-03T00:00:00Z",
    },
    "lockfile": {"dependency-count": 1},
    "settings": {},
}
CARGO_AUDIT_WARNING_RESULT = {
    **CARGO_AUDIT_METADATA,
    "vulnerabilities": {"count": 0, "found": False, "list": []},
    "warnings": {
        "unmaintained": [
            {
                "advisory": {"id": "RUSTSEC-2024-0436"},
                "package": {"name": "paste", "version": "1.0.15"},
            }
        ]
    },
}
CARGO_AUDIT_VULNERABILITY_RESULT = {
    **CARGO_AUDIT_METADATA,
    "vulnerabilities": {
        "count": 1,
        "found": True,
        "list": [
            {
                "advisory": {"id": "RUSTSEC-2099-0001"},
                "package": {"name": "example", "version": "1.0.0"},
            }
        ],
    },
    "warnings": {},
}
NPM_AUDIT_CLEAN_RESULT = {
    "auditReportVersion": 2,
    "metadata": {"vulnerabilities": {"total": 0}},
    "vulnerabilities": {},
}
NPM_AUDIT_FINDING_RESULT = {
    "auditReportVersion": 2,
    "metadata": {"vulnerabilities": {"total": 1}},
    "vulnerabilities": {"example": {"severity": "high"}},
}
PIP_AUDIT_CLEAN_RESULT = {
    "dependencies": [{"name": "example", "version": "1.0.0", "vulns": []}],
    "fixes": [],
}
PIP_AUDIT_FINDING_RESULT = {
    "dependencies": [
        {
            "name": "example",
            "version": "1.0.0",
            "vulns": [{"id": "PYSEC-2099-1"}],
        }
    ],
    "fixes": [],
}
PIP_AUDIT_DUPLICATE_RESULT = {
    "dependencies": [
        {
            "name": "example",
            "version": "1.0.0",
            "vulns": [
                {"id": "PYSEC-2099-1", "fix_versions": ["1.0.1"]},
                {"id": "GHSA-2099-0002"},
                {"id": "PYSEC-2099-1", "fix_versions": ["1.0.1.0"]},
            ],
        }
    ],
    "fixes": [],
}
ZIZMOR_LOW_CONFIDENCE_RESULT = [
    {
        "ident": "cache-poisoning",
        "determinations": {"confidence": "Low", "severity": "High"},
        "locations": [{}],
    }
]
ZIZMOR_LOCATED_RESULT = [
    {
        "ident": "cache-poisoning",
        "determinations": {"confidence": "Low", "severity": "High"},
        "locations": [
            {
                "symbolic": {
                    "key": {
                        "Local": {"verbatim_path": "./.github/workflows/other.yml"}
                    },
                    "kind": "Related",
                },
                "concrete": {"location": {"start_point": {"row": 1}}},
            },
            {
                "symbolic": {
                    "key": {"Local": {"verbatim_path": "./.github/workflows/ci.yml"}},
                    "kind": "Primary",
                },
                "concrete": {"location": {"start_point": {"row": 11}}},
            },
        ],
    }
]
ZIZMOR_BLOCKING_CONFIDENCE_RESULTS = (
    [
        {
            "ident": "example-medium",
            "determinations": {"confidence": "Medium", "severity": "Medium"},
            "locations": [{}],
        }
    ],
    [
        {
            "ident": "example-high",
            "determinations": {"confidence": "High", "severity": "High"},
            "locations": [{}],
        }
    ],
)
SCANNER_CLASSIFIERS = {
    "cargo-audit": security_audit.classify_cargo_audit_result,
    "npm-audit": security_audit.classify_npm_audit_result,
    "pip-audit": security_audit.classify_pip_audit_result,
    "zizmor": security_audit.classify_zizmor_result,
}
INCOMPLETE_SCANNER_CASES = (
    pytest.param(
        "cargo-audit",
        2,
        json.dumps(CARGO_AUDIT_WARNING_RESULT),
        "unexpected exit code 2",
        id="cargo-audit-exit-code",
    ),
    pytest.param(
        "npm-audit",
        2,
        json.dumps(NPM_AUDIT_CLEAN_RESULT),
        "unexpected exit code 2",
        id="npm-audit-exit-code",
    ),
    pytest.param(
        "pip-audit",
        2,
        json.dumps(PIP_AUDIT_CLEAN_RESULT),
        "unexpected exit code 2",
        id="pip-audit-exit-code",
    ),
    pytest.param(
        "zizmor",
        1,
        json.dumps(ZIZMOR_LOW_CONFIDENCE_RESULT),
        "unexpected exit code 1",
        id="zizmor-exit-code",
    ),
    pytest.param(
        "pip-audit",
        1,
        json.dumps(PIP_AUDIT_CLEAN_RESULT),
        "exit code 1 without reported findings",
        id="pip-audit-failure-without-findings",
    ),
    pytest.param(
        "cargo-audit",
        0,
        "{}",
        "invalid database, lockfile, or settings metadata",
        id="cargo-audit-invalid-report",
    ),
    pytest.param(
        "npm-audit",
        0,
        "{}",
        "invalid auditReportVersion",
        id="npm-audit-invalid-report",
    ),
    pytest.param(
        "pip-audit",
        0,
        "{}",
        "invalid dependencies or fixes list",
        id="pip-audit-invalid-report",
    ),
    pytest.param(
        "zizmor",
        0,
        "{}",
        "expected a JSON array",
        id="zizmor-invalid-report",
    ),
    pytest.param(
        "zizmor",
        0,
        json.dumps([{"determinations": {"confidence": "Low", "severity": "High"}}]),
        "invalid finding entry",
        id="zizmor-malformed-list",
    ),
    pytest.param(
        "npm-audit",
        1,
        json.dumps({"error": {"code": "ENOAUDIT"}}),
        'scanner error: {"code": "ENOAUDIT"}',
        id="npm-audit-scanner-error",
    ),
    pytest.param(
        "npm-audit",
        1,
        "npm error audit endpoint returned an error",
        "invalid JSON output: Expecting value: line 1 column 1 (char 0)",
        id="npm-audit-unparseable",
    ),
)


def test_scanner_json_error_is_incomplete() -> None:
    error_result = {"error": {"code": "network"}}
    for classifier in (
        security_audit.classify_cargo_audit_result,
        security_audit.classify_npm_audit_result,
        security_audit.classify_pip_audit_result,
        security_audit.classify_zizmor_result,
    ):
        assert classifier(1, error_result, None).status == "incomplete"


def test_scanner_result_classification() -> None:
    assert (
        security_audit.classify_npm_audit_result(0, NPM_AUDIT_CLEAN_RESULT, None).status
        == "clean"
    )
    assert (
        security_audit.classify_npm_audit_result(
            0, NPM_AUDIT_FINDING_RESULT, None
        ).status
        == "findings"
    )
    assert (
        security_audit.classify_pip_audit_result(0, PIP_AUDIT_CLEAN_RESULT, None).status
        == "clean"
    )
    assert (
        security_audit.classify_pip_audit_result(
            0, PIP_AUDIT_FINDING_RESULT, None
        ).status
        == "findings"
    )
    assert not security_audit.policy_passes(["findings"])


def test_cargo_audit_unmaintained_warnings_are_non_blocking() -> None:
    status = security_audit.classify_cargo_audit_result(
        0, CARGO_AUDIT_WARNING_RESULT, None
    ).status

    assert status == "warnings"
    assert security_audit.policy_passes([status])


def test_cargo_audit_vulnerabilities_remain_blocking() -> None:
    status = security_audit.classify_cargo_audit_result(
        1, CARGO_AUDIT_VULNERABILITY_RESULT, None
    ).status

    assert status == "findings"
    assert not security_audit.policy_passes([status])


def test_zizmor_confidence_controls_enforcement() -> None:
    low_status = security_audit.classify_zizmor_result(
        0, ZIZMOR_LOW_CONFIDENCE_RESULT, None
    ).status
    assert low_status == "warnings"
    assert security_audit.policy_passes(["clean", low_status])
    for blocking_result in ZIZMOR_BLOCKING_CONFIDENCE_RESULTS:
        blocking_status = security_audit.classify_zizmor_result(
            0, blocking_result, None
        ).status
        assert blocking_status == "findings"
        assert not security_audit.policy_passes([blocking_status])


def test_zizmor_tool_errors_remain_blocking() -> None:
    status = security_audit.classify_zizmor_result(
        1, ZIZMOR_LOW_CONFIDENCE_RESULT, None
    ).status

    assert status == "incomplete"
    assert not security_audit.policy_passes([status])


def test_specialized_scanners_fail_closed_on_malformed_output() -> None:
    assert (
        security_audit.classify_cargo_audit_result(0, {}, None).status == "incomplete"
    )
    malformed_cargo_warnings = {
        **CARGO_AUDIT_METADATA,
        "vulnerabilities": {"count": 0, "found": False, "list": []},
        "warnings": {"unmaintained": [{}]},
    }
    assert (
        security_audit.classify_cargo_audit_result(
            0, malformed_cargo_warnings, None
        ).status
        == "incomplete"
    )
    assert security_audit.classify_npm_audit_result(0, {}, None).status == "incomplete"
    assert security_audit.classify_pip_audit_result(0, {}, None).status == "incomplete"
    assert (
        security_audit.classify_zizmor_result(
            0,
            [{"determinations": {"confidence": "Low", "severity": "High"}}],
            None,
        ).status
        == "incomplete"
    )


def test_security_findings_are_described_without_scanner_output() -> None:
    assert security_audit.describe_findings(
        "cargo-audit", "findings", CARGO_AUDIT_VULNERABILITY_RESULT
    ) == ("vulnerability RUSTSEC-2099-0001 `example` 1.0.0",)
    assert security_audit.describe_findings(
        "cargo-audit", "warnings", CARGO_AUDIT_WARNING_RESULT
    ) == ("unmaintained RUSTSEC-2024-0436 `paste` 1.0.15",)
    assert security_audit.describe_findings(
        "npm-audit", "findings", NPM_AUDIT_FINDING_RESULT
    ) == ("`example` (high)",)
    assert security_audit.describe_findings(
        "pip-audit", "findings", PIP_AUDIT_FINDING_RESULT
    ) == ("`example` 1.0.0: PYSEC-2099-1",)
    assert security_audit.describe_findings(
        "pip-audit", "findings", PIP_AUDIT_DUPLICATE_RESULT
    ) == ("`example` 1.0.0: PYSEC-2099-1, GHSA-2099-0002",)
    assert security_audit.describe_findings(
        "zizmor", "warnings", ZIZMOR_LOCATED_RESULT
    ) == (
        "cache-poisoning (Low confidence, High severity) at "
        "./.github/workflows/ci.yml:12",
    )
    assert security_audit.describe_findings(
        "zizmor", "warnings", ZIZMOR_LOW_CONFIDENCE_RESULT
    ) == ("cache-poisoning (Low confidence, High severity)",)
    assert (
        security_audit.describe_findings("npm-audit", "clean", NPM_AUDIT_CLEAN_RESULT)
        == ()
    )


def test_incomplete_results_report_errors_as_reasons_not_findings() -> None:
    scanner_error = {"error": {"code": "ENOAUDIT"}}

    assert security_audit.classify_npm_audit_result(1, scanner_error, None) == (
        "incomplete",
        'scanner error: {"code": "ENOAUDIT"}',
    )
    assert (
        security_audit.describe_findings("npm-audit", "incomplete", scanner_error) == ()
    )
    assert security_audit.describe_findings("npm-audit", "incomplete", None) == ()


def test_security_summary_reports_details_in_log_and_step_summary(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    step_summary = tmp_path / "step-summary.md"
    monkeypatch.setattr(security_audit, "REPORT_DIRECTORY", tmp_path)
    monkeypatch.setenv("GITHUB_STEP_SUMMARY", str(step_summary))
    detail_count = security_audit.MAX_DETAILS_PER_SCANNER + 2
    results = [
        security_audit.ScanResult(
            "cargo-audit",
            ["cargo", "audit", "--json"],
            ".",
            0,
            "warnings",
            "cargo-audit.json",
            details=tuple(
                f"unmaintained RUSTSEC-{index}" for index in range(detail_count)
            ),
        ),
        security_audit.ScanResult(
            "pip-audit",
            ["pip-audit"],
            ".",
            None,
            "incomplete",
            "pip-audit.json",
            "scanner produced no JSON output",
        ),
    ]

    security_audit.write_summary(results)

    logged = capsys.readouterr().out
    assert step_summary.read_text(encoding="utf-8") == logged
    assert (tmp_path / "summary.md").read_text(encoding="utf-8") == logged
    assert "| `cargo-audit` | warnings | 0 |" in logged
    assert "- `cargo-audit`: unmaintained RUSTSEC-0" in logged
    assert "- `cargo-audit`: 2 more" in logged
    assert f"RUSTSEC-{security_audit.MAX_DETAILS_PER_SCANNER}\n" not in logged
    assert "- `pip-audit`: error: scanner produced no JSON output" in logged


@pytest.mark.parametrize(
    ("name", "exit_code", "stdout", "reason"), INCOMPLETE_SCANNER_CASES
)
def test_incomplete_classification_records_a_reason(
    name: str, exit_code: int, stdout: str, reason: str
) -> None:
    classifier = SCANNER_CLASSIFIERS[name]

    classification = classifier(exit_code, *security_audit.parse_json_output(stdout))

    assert classification == ("incomplete", reason)
    assert not security_audit.policy_passes([classification.status])


def fake_scanner_run(
    monkeypatch: pytest.MonkeyPatch, exit_code: int, stdout: str, stderr: str
) -> None:
    def run(command: list[str], **_: object) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(command, exit_code, stdout, stderr)

    monkeypatch.setattr(security_audit.subprocess, "run", run)


@pytest.mark.parametrize(
    ("name", "exit_code", "stdout", "reason"), INCOMPLETE_SCANNER_CASES
)
def test_incomplete_scanner_summary_reports_reason_and_stderr(
    name: str,
    exit_code: int,
    stdout: str,
    reason: str,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.setattr(security_audit, "REPORT_DIRECTORY", tmp_path)
    monkeypatch.delenv("GITHUB_STEP_SUMMARY", raising=False)
    stderr = "".join(f"diagnostic {index}\n" for index in range(30))
    fake_scanner_run(monkeypatch, exit_code, stdout, stderr)

    result = security_audit.run_command(
        name, [name], security_audit.REPOSITORY_ROOT, SCANNER_CLASSIFIERS[name]
    )
    security_audit.write_summary([result])

    logged = capsys.readouterr().out
    assert result.status == "incomplete"
    assert f"| `{name}` | incomplete | {exit_code} |" in logged
    assert f"- `{name}`: error: {reason}\n" in logged
    first_kept = 30 - security_audit.MAX_STDERR_LINES
    assert f"diagnostic {first_kept}\n" in logged
    assert "diagnostic 29\n" in logged
    assert f"diagnostic {first_kept - 1}\n" not in logged


def test_complete_scanner_summary_omits_stderr(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.setattr(security_audit, "REPORT_DIRECTORY", tmp_path)
    monkeypatch.delenv("GITHUB_STEP_SUMMARY", raising=False)
    fake_scanner_run(
        monkeypatch, 0, json.dumps(CARGO_AUDIT_WARNING_RESULT), "Fetching advisories\n"
    )

    result = security_audit.run_command(
        "cargo-audit",
        ["cargo-audit"],
        security_audit.REPOSITORY_ROOT,
        security_audit.classify_cargo_audit_result,
    )
    security_audit.write_summary([result])

    assert result.status == "warnings"
    assert result.error is None
    assert result.stderr_excerpt is None
    assert "Fetching advisories" not in capsys.readouterr().out


def test_stderr_excerpt_keeps_a_bounded_tail() -> None:
    lines = [f"diagnostic {index}" for index in range(50)]

    assert security_audit.stderr_excerpt("\n".join(lines) + "\n") == "\n".join(
        lines[-security_audit.MAX_STDERR_LINES :]
    )
    long_excerpt = security_audit.stderr_excerpt(
        "x" * (2 * security_audit.MAX_STDERR_CHARACTERS) + "end"
    )
    assert long_excerpt is not None
    assert len(long_excerpt) == security_audit.MAX_STDERR_CHARACTERS
    assert long_excerpt.endswith("end")
    assert security_audit.stderr_excerpt(" \n\n") is None


def test_stderr_excerpt_hides_tokens_and_workflow_commands(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.setattr(security_audit, "REPORT_DIRECTORY", tmp_path)
    monkeypatch.delenv("GITHUB_STEP_SUMMARY", raising=False)
    monkeypatch.setenv("GH_TOKEN", "example-token-value")
    excerpt = security_audit.stderr_excerpt(
        "request with example-token-value failed\n::error::spoofed\n```\n"
    )

    security_audit.write_summary(
        [
            security_audit.ScanResult(
                "zizmor",
                ["zizmor"],
                ".",
                1,
                "incomplete",
                "zizmor.json",
                "unexpected exit code 1",
                stderr_excerpt=excerpt,
            )
        ]
    )

    logged = capsys.readouterr().out
    assert "example-token-value" not in logged
    assert "request with *** failed" in logged
    assert "::error::spoofed" in logged
    stripped_lines = [line.strip() for line in logged.splitlines()]
    assert not any(line.startswith("::") for line in stripped_lines)
    assert stripped_lines.count("```") == 1


def test_python_lock_export_failure_reports_bounded_stderr(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(security_audit, "REPORT_DIRECTORY", tmp_path)
    fake_scanner_run(monkeypatch, 2, "", "error: lockfile needs to be updated\n")

    result = security_audit.scan_python_lock()

    assert result.status == "incomplete"
    assert result.error == "uv export exited 2"
    assert result.stderr_excerpt == "error: lockfile needs to be updated"


def test_rust_future_incompatibility_names_packages() -> None:
    output = (
        "warning: the following packages contain code that will be rejected by a "
        "future version of Rust: nom v1.2.4, rental v0.5.6\n"
    )

    assert classify_rust_future_incompatibility(0, output) == (
        [
            Finding(
                "Rust",
                "workspace",
                "locked",
                "future-incompatible code in nom v1.2.4, rental v0.5.6",
            )
        ],
        [],
    )
    assert classify_rust_future_incompatibility(0, "Finished dev profile\n") == ([], [])


def test_rust_future_incompatibility_failure_names_the_first_error() -> None:
    output = (
        "   Compiling leptonica-sys v0.4.9\n"
        "error: failed to run custom build command for `leptonica-sys v0.4.9`\n"
        "error: could not compile workspace\n"
    )

    assert classify_rust_future_incompatibility(101, output) == (
        [],
        [
            "Rust future-incompatibility check exited 101: "
            "error: failed to run custom build command for `leptonica-sys v0.4.9`"
        ],
    )
    assert classify_rust_future_incompatibility(101, "") == (
        [],
        ["Rust future-incompatibility check exited 101"],
    )


def test_deprecation_summary_is_logged(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.setattr(deprecation_report, "REPORT_DIRECTORY", tmp_path)
    monkeypatch.delenv("GITHUB_STEP_SUMMARY", raising=False)

    deprecation_report.write_summary(
        [Finding("Node", "example", "1.0.0", "deprecated")], ["npm metadata: timeout"]
    )

    logged = capsys.readouterr().out
    assert (tmp_path / "summary.md").read_text(encoding="utf-8") == logged
    assert "| Node | `example` | `1.0.0` | deprecated |" in logged
    assert "- npm metadata: timeout" in logged


def test_exact_version_parsing() -> None:
    assert exact_version("=1.2.3") == "1.2.3"
    assert exact_version("==1.2.3") == "1.2.3"
    assert exact_version("^1.2.3") is None


def test_year_month_uses_the_last_day() -> None:
    assert parse_python_lifecycle_date("2026-10") == date(2026, 10, 31)
    assert parse_python_lifecycle_date("2028-02") == date(2028, 2, 29)


def test_python_lifecycle_parser_accepts_full_dates() -> None:
    assert parse_python_lifecycle_date("2026-10-31") == date(2026, 10, 31)


def test_deprecation_manifest_records_reproducibility_inputs() -> None:
    versions = {
        "rustc": "rustc 1.97.1",
        "cargo": "cargo 1.97.1",
        "python": "Python 3.14.0",
        "node": "v24.18.0",
        "npm": "11.16.0",
        "uv": "uv 0.11.29",
    }

    manifest = build_manifest([], [], versions, checked_at="2026-07-20T00:00:00+00:00")

    assert manifest["checked_at"] == "2026-07-20T00:00:00+00:00"
    assert manifest["tool_versions"] == versions
    assert manifest["audited_inputs"] == [
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
    ]
