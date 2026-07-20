"""Repository CI policy tests."""

from __future__ import annotations

import json
import re
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
WORKFLOW_DIRECTORY = REPOSITORY_ROOT / ".github" / "workflows"
FULL_SHA_PATTERN = re.compile(
    r"^\s*uses:\s*[^\s@]+@([0-9a-f]{40})\s*(?:#.*)?$", re.MULTILINE
)
ANY_ACTION_PATTERN = re.compile(r"^\s*uses:\s*[^\s@]+@([^\s#]+)", re.MULTILINE)
BERYL_RUNNER = "[self-hosted, linux, x64, beryl]"
NODE_ENGINE = ">=22"
SETUP_GO_SHA = "4a3601121dd01d1626a1e23e37211e3254c1c06c"


def read_workflow(name: str) -> str:
    return (WORKFLOW_DIRECTORY / name).read_text(encoding="utf-8")


def test_expected_workflows_replace_legacy_ci() -> None:
    assert sorted(path.name for path in WORKFLOW_DIRECTORY.glob("*.yml")) == [
        "dependency-health.yml",
        "linux-ci.yml",
        "nightly.yml",
    ]
    assert not (REPOSITORY_ROOT / ".circleci" / "config.yml").exists()
    assert not (REPOSITORY_ROOT / "codecov.yml").exists()


def test_linux_ci_contract() -> None:
    workflow = read_workflow("linux-ci.yml")

    assert "name: Linux CI" in workflow
    assert "pull_request:" in workflow and "branches: [master]" in workflow
    assert "tags: ['v[0-9]+.[0-9]+.[0-9]+']" in workflow
    assert "workflow_dispatch:" in workflow
    assert "name: Linux / ${{ matrix.name }}" in workflow
    assert "name: Linux / Required" in workflow
    assert 'rust: "1.97.1"' in workflow
    assert 'python: "3.14"' in workflow
    assert 'node: "24.18.0"' in workflow
    assert 'rust: "1.88.0"' in workflow
    assert 'python: "3.10"' in workflow
    assert 'node: "22.23.1"' in workflow
    assert "max-parallel: 1" in workflow
    assert "fail-fast: false" in workflow
    assert f"runs-on: {BERYL_RUNNER}" in workflow
    assert "head.repo.full_name == github.repository" in workflow
    assert "cancel-in-progress: ${{ github.event_name == 'pull_request' }}" in workflow
    assert "timeout-minutes: 30" in workflow
    assert "timeout-minutes: 5" in workflow
    assert "make ${{ matrix.make_target }}" in workflow
    assert "needs: supported-versions" in workflow
    assert "if: always()" in workflow


def test_scheduled_workflow_contracts() -> None:
    nightly = read_workflow("nightly.yml")
    dependency_health = read_workflow("dependency-health.yml")
    audit_step = dependency_health.partition(
        "      - name: Run all security scanners\n"
    )[2].partition("\n      - name:")[0]

    assert "cron: '17 6 * * *'" in nightly
    assert f"runs-on: {BERYL_RUNNER}" in nightly
    assert "make ci" in nightly and "make release" in nightly
    assert "timeout-minutes: 30" in nightly
    assert "cron: '37 6 * * 1'" in dependency_health
    assert "name: Dependency / Security audit" in dependency_health
    assert "name: Dependency / Deprecation report" in dependency_health
    assert "target/ci-reports/security" in dependency_health
    assert "target/ci-reports/deprecations" in dependency_health
    assert "retention-days: 7" in dependency_health
    assert "timeout-minutes: 20" in dependency_health
    assert "timeout-minutes: 30" in dependency_health
    assert "GH_TOKEN: ${{ github.token }}" in audit_step


def test_workflows_are_read_only_and_actions_are_pinned() -> None:
    for workflow_path in WORKFLOW_DIRECTORY.glob("*.yml"):
        workflow = workflow_path.read_text(encoding="utf-8")
        assert re.search(r"^permissions:\n  contents: read$", workflow, re.MULTILINE)
        assert "persist-credentials: false" in workflow
        action_references = ANY_ACTION_PATTERN.findall(workflow)
        full_sha_references = FULL_SHA_PATTERN.findall(workflow)
        assert action_references
        assert len(action_references) == len(full_sha_references)
        assert "pull_request_target" not in workflow


def test_actionlint_uses_a_supported_pinned_installer() -> None:
    for workflow_name in ("linux-ci.yml", "nightly.yml"):
        workflow = read_workflow(workflow_name)

        assert f"actions/setup-go@{SETUP_GO_SHA} # v6.4.0" in workflow
        assert 'go-version: "1.26.5"' in workflow
        assert "cache: false" in workflow
        assert (
            "go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.12" in workflow
        )
        assert "tool: actionlint@1.7.12" not in workflow


def test_ci_is_linux_only() -> None:
    workflow_text = "\n".join(
        path.read_text(encoding="utf-8") for path in WORKFLOW_DIRECTORY.glob("*.yml")
    ).lower()

    for unsupported_runner in ("windows-", "macos-", "cuda", "arm64"):
        assert unsupported_runner not in workflow_text


def test_node_support_floor_is_consistent() -> None:
    package = json.loads(
        (REPOSITORY_ROOT / "bindings" / "node" / "package.json").read_text(
            encoding="utf-8"
        )
    )
    lock = json.loads(
        (REPOSITORY_ROOT / "bindings" / "node" / "package-lock.json").read_text(
            encoding="utf-8"
        )
    )

    assert package["engines"]["node"] == NODE_ENGINE
    assert lock["packages"][""]["engines"]["node"] == NODE_ENGINE
