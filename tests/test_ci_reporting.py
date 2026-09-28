"""Unit tests for CI report classification and lifecycle parsing."""

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


def test_scanner_json_error_is_incomplete() -> None:
    error_result = {"error": {"code": "network"}}
    for classifier in (
        security_audit.classify_cargo_audit_result,
        security_audit.classify_npm_audit_result,
        security_audit.classify_pip_audit_result,
        security_audit.classify_zizmor_result,
    ):
        assert classifier(1, error_result, None) == "incomplete"


def test_scanner_result_classification() -> None:
    assert (
        security_audit.classify_npm_audit_result(0, NPM_AUDIT_CLEAN_RESULT, None)
        == "clean"
    )
    assert (
        security_audit.classify_npm_audit_result(0, NPM_AUDIT_FINDING_RESULT, None)
        == "findings"
    )
    assert (
        security_audit.classify_pip_audit_result(0, PIP_AUDIT_CLEAN_RESULT, None)
        == "clean"
    )
    assert (
        security_audit.classify_pip_audit_result(0, PIP_AUDIT_FINDING_RESULT, None)
        == "findings"
    )
    assert not security_audit.policy_passes(["findings"])


def test_cargo_audit_unmaintained_warnings_are_non_blocking() -> None:
    status = security_audit.classify_cargo_audit_result(
        0, CARGO_AUDIT_WARNING_RESULT, None
    )

    assert status == "warnings"
    assert security_audit.policy_passes([status])


def test_cargo_audit_vulnerabilities_remain_blocking() -> None:
    status = security_audit.classify_cargo_audit_result(
        1, CARGO_AUDIT_VULNERABILITY_RESULT, None
    )

    assert status == "findings"
    assert not security_audit.policy_passes([status])


def test_zizmor_confidence_controls_enforcement() -> None:
    low_status = security_audit.classify_zizmor_result(
        0, ZIZMOR_LOW_CONFIDENCE_RESULT, None
    )
    assert low_status == "warnings"
    assert security_audit.policy_passes(["clean", low_status])
    for blocking_result in ZIZMOR_BLOCKING_CONFIDENCE_RESULTS:
        blocking_status = security_audit.classify_zizmor_result(
            0, blocking_result, None
        )
        assert blocking_status == "findings"
        assert not security_audit.policy_passes([blocking_status])


def test_zizmor_tool_errors_remain_blocking() -> None:
    status = security_audit.classify_zizmor_result(
        1, ZIZMOR_LOW_CONFIDENCE_RESULT, None
    )

    assert status == "incomplete"
    assert not security_audit.policy_passes([status])


def test_specialized_scanners_fail_closed_on_malformed_output() -> None:
    assert security_audit.classify_cargo_audit_result(0, {}, None) == "incomplete"
    malformed_cargo_warnings = {
        **CARGO_AUDIT_METADATA,
        "vulnerabilities": {"count": 0, "found": False, "list": []},
        "warnings": {"unmaintained": [{}]},
    }
    assert (
        security_audit.classify_cargo_audit_result(0, malformed_cargo_warnings, None)
        == "incomplete"
    )
    assert security_audit.classify_npm_audit_result(0, {}, None) == "incomplete"
    assert security_audit.classify_pip_audit_result(0, {}, None) == "incomplete"
    assert (
        security_audit.classify_zizmor_result(
            0,
            [{"determinations": {"confidence": "Low", "severity": "High"}}],
            None,
        )
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


def test_incomplete_scanner_error_is_described() -> None:
    assert security_audit.describe_findings(
        "npm-audit", "incomplete", {"error": {"code": "ENOAUDIT"}}
    ) == ('scanner error: {"code": "ENOAUDIT"}',)
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
