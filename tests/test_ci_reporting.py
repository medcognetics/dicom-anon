"""Unit tests for CI report classification and lifecycle parsing."""

from datetime import date

from tools.ci.deprecation_report import build_manifest, exact_version, parse_year_month
from tools.ci.security_audit import classify_result


def test_scanner_json_error_is_incomplete() -> None:
    assert classify_result(1, {"error": {"code": "network"}}, None) == "incomplete"


def test_scanner_result_classification() -> None:
    assert classify_result(0, {}, None) == "clean"
    assert classify_result(1, {"vulnerabilities": ["example"]}, None) == "findings"
    assert classify_result(1, None, "invalid JSON") == "incomplete"


def test_exact_version_parsing() -> None:
    assert exact_version("=1.2.3") == "1.2.3"
    assert exact_version("==1.2.3") == "1.2.3"
    assert exact_version("^1.2.3") is None


def test_year_month_uses_the_last_day() -> None:
    assert parse_year_month("2026-10") == date(2026, 10, 31)
    assert parse_year_month("2028-02") == date(2028, 2, 29)


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
