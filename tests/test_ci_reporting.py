"""Unit tests for CI report classification and lifecycle parsing."""

from datetime import date

from tools.ci import security_audit
from tools.ci.deprecation_report import (
    build_manifest,
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
ZIZMOR_LOW_CONFIDENCE_RESULT = [
    {
        "ident": "cache-poisoning",
        "determinations": {"confidence": "Low", "severity": "High"},
        "locations": [{}],
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
