"""Repository CI policy tests."""

from __future__ import annotations

import itertools
import json
import re
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
WORKFLOW_DIRECTORY = REPOSITORY_ROOT / ".github" / "workflows"
FULL_SHA_PATTERN = re.compile(
    r"^\s*uses:\s*[^\s@]+@([0-9a-f]{40})\s*(?:#.*)?$", re.MULTILINE
)
ANY_ACTION_PATTERN = re.compile(r"^\s*uses:\s*[^\s@]+@([^\s#]+)", re.MULTILINE)
# The repository is public. The org's self-hosted runner group does not accept public
# repositories, and hosted runners isolate fork pull requests, so CI runs on hosted Linux.
HOSTED_RUNNER = "ubuntu-24.04"
JOB_PATTERN = re.compile(r"^  (?P<job>[A-Za-z0-9_-]+):$")
RUNS_ON_PATTERN = re.compile(r"^    runs-on:[ \t]*(?P<value>.*)$")
BLOCK_SCALAR_PATTERN = re.compile(r"^[>|][-+]?(?:[ \t]+#.*)?$")
NODE_ENGINE = ">=22"
MINIMUM_SAFE_JS_YAML_VERSION = (4, 3, 1)
RUST_TOOLCHAIN_ACTION = "dtolnay/rust-toolchain"
RUST_TOOLCHAIN_ACTION_SHA = "6c977a6ca4077a0ceb28ffbe03f59d46e9ac8772"
SETUP_GO_SHA = "4a3601121dd01d1626a1e23e37211e3254c1c06c"
CI_CONTAINER_IMAGE = "ubuntu:24.04@sha256:4fbb8e6a8395de5a7550b33509421a2bafbc0aab6c06ba2cef9ebffbc7092d90"
CI_CONTAINER_PACKAGES = (
    "build-essential",
    "ca-certificates",
    "clang",
    "curl",
    "git",
    "libclang-dev",
    "libleptonica-dev",
    "libtesseract-dev",
    "pkg-config",
    "tesseract-ocr",
    "tesseract-ocr-eng",
)
DEPRECATION_NATIVE_PACKAGES = (
    "clang",
    "libclang-dev",
    "libleptonica-dev",
    "libtesseract-dev",
    "pkg-config",
)
ZIZMOR_VERSION = "1.29.0"


def workflow_paths() -> list[Path]:
    """Every workflow file. GitHub Actions runs both `.yml` and `.yaml` files."""
    return sorted(
        [*WORKFLOW_DIRECTORY.glob("*.yml"), *WORKFLOW_DIRECTORY.glob("*.yaml")]
    )


def read_workflow(name: str) -> str:
    return (WORKFLOW_DIRECTORY / name).read_text(encoding="utf-8")


def job_runners(workflow: str) -> dict[str, str]:
    """Return each job's runs-on value and join folded block scalars."""
    lines = workflow.partition("\njobs:\n")[2].splitlines()
    runners: dict[str, str] = {}
    job = None
    for index, line in enumerate(lines):
        if job_match := JOB_PATTERN.match(line):
            job = job_match["job"]
        elif job is not None and (runs_on_match := RUNS_ON_PATTERN.match(line)):
            value = runs_on_match["value"].strip()
            if BLOCK_SCALAR_PATTERN.match(value):
                continuation = itertools.takewhile(
                    lambda text: text.startswith("      "), lines[index + 1 :]
                )
                value = " ".join(text.strip() for text in continuation)
            runners[job] = value
    return runners


def test_expected_workflows_replace_legacy_ci() -> None:
    assert [path.name for path in workflow_paths()] == [
        "dependency-health.yml",
        "linux-ci.yml",
        "nightly.yml",
        "release.yml",
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
    assert "fail-fast: false" in workflow
    assert "cancel-in-progress: ${{ github.event_name == 'pull_request' }}" in workflow
    assert "timeout-minutes: 30" in workflow
    assert "timeout-minutes: 5" in workflow
    assert "make ${{ matrix.make_target }}" in workflow
    assert "needs: [supported-versions, report-cpu]" in workflow
    assert "if: always()" in workflow


def test_linux_ci_runs_every_pull_request_on_hosted_runners() -> None:
    workflow = read_workflow("linux-ci.yml")

    assert job_runners(workflow) == {
        "supported-versions": HOSTED_RUNNER,
        "report-cpu": HOSTED_RUNNER,
        "required": HOSTED_RUNNER,
    }
    # Fork pull requests run too: hosted runners are ephemeral, and the jobs have a
    # read-only token and no secrets.
    assert "head.repo.full_name" not in workflow
    assert "secrets." not in workflow


def test_every_job_runs_on_a_hosted_runner() -> None:
    # A self-hosted runner on a public repository lets a fork pull request run its own
    # workflow there (#45), so every job must use the hosted image.
    for workflow_path in workflow_paths():
        workflow = workflow_path.read_text(encoding="utf-8")
        jobs = [
            match["job"]
            for line in workflow.partition("\njobs:\n")[2].splitlines()
            if (match := JOB_PATTERN.match(line))
        ]
        runners = job_runners(workflow)
        # A job without runs-on (for example, a reusable-workflow call) is not allowed.
        assert jobs and sorted(runners) == sorted(jobs), (workflow_path.name, jobs)
        assert set(runners.values()) == {HOSTED_RUNNER}, (workflow_path.name, runners)
    assert "labels: []" in (REPOSITORY_ROOT / ".github" / "actionlint.yaml").read_text(
        encoding="utf-8"
    )


def test_ci_jobs_use_a_pinned_ci_container() -> None:
    for workflow_name in ("linux-ci.yml", "nightly.yml"):
        workflow = read_workflow(workflow_name)
        prerequisite_step = workflow.partition(
            "      - name: Install container prerequisites\n"
        )[2].partition("\n      - name:")[0]

        assert f"image: {CI_CONTAINER_IMAGE}" in workflow
        assert "shell: bash" in workflow
        assert prerequisite_step
        assert "apt-get update" in prerequisite_step
        assert "pkg-config --exists lept tesseract" in prerequisite_step
        assert "sudo" not in prerequisite_step
        for package in CI_CONTAINER_PACKAGES:
            assert re.search(
                rf"^[ \t]+{re.escape(package)}(?:[ \t]+\\)?[ \t]*$",
                prerequisite_step,
                re.MULTILINE,
            )


def test_scheduled_workflow_contracts() -> None:
    nightly = read_workflow("nightly.yml")
    dependency_health = read_workflow("dependency-health.yml")
    audit_step = dependency_health.partition(
        "      - name: Run all security scanners\n"
    )[2].partition("\n      - name:")[0]

    assert "cron: '17 6 * * *'" in nightly
    assert job_runners(nightly)["validate-release"] == HOSTED_RUNNER
    assert "make ci" in nightly and "make release" in nightly
    assert "timeout-minutes: 30" in nightly
    assert "cron: '37 6 * * 1'" in dependency_health
    assert "name: Dependency / Security audit" in dependency_health
    assert "name: Dependency / Deprecation report" in dependency_health
    assert "run: make audit" in dependency_health
    assert "run: make deprecations" in dependency_health
    assert "timeout-minutes: 20" in dependency_health
    assert "timeout-minutes: 30" in dependency_health
    assert "GH_TOKEN: ${{ github.token }}" in audit_step


def test_release_workflow_contract() -> None:
    workflow = read_workflow("release.yml")
    job_permissions = workflow.partition("    permissions:\n")[2].partition("    env:")[
        0
    ]
    publish_step = workflow.partition("      - name: Create draft GitHub release\n")[2]

    assert job_runners(workflow) == {"release": HOSTED_RUNNER}
    assert f"image: {CI_CONTAINER_IMAGE}" in workflow
    assert "tags: ['v[0-9]+.[0-9]+.[0-9]+']" in workflow
    assert "pull_request" not in workflow
    assert job_permissions.split() == [
        "contents:",
        "write",
        "actions:",
        "read",
        "checks:",
        "read",
    ]
    # Release builds install pinned, verified tools and use no cache that another job
    # can write.
    for shared_cache_action in (
        "rust-cache",
        "actions/setup-python@",
        "actions/setup-node@",
        "astral-sh/setup-uv@",
        "dtolnay/rust-toolchain@",
    ):
        assert shared_cache_action not in workflow
    assert 'tools="${RUNNER_TEMP}/release-tools"' in workflow
    assert 'rm -rf "${tools}"' in workflow
    for checksum in ("UV_SHA256", "NODE_SHA256", "RUSTUP_SHA256"):
        assert re.search(
            rf"^          {checksum}: [0-9a-f]{{64}}$", workflow, re.MULTILINE
        )
        assert f'verify "${{{checksum}}}"' in workflow
    assert 'echo "UV_PYTHON_PREFERENCE=only-managed"' in workflow
    assert "tool: cargo-audit@0.22.2,cargo-cyclonedx@0.5.9" in workflow
    assert "run: make audit" in workflow
    assert "release.py check-version --tag" in workflow
    assert "release.py build --tag" in workflow
    assert "sha256sum --check SHA256SUMS" in workflow
    for precondition in (
        "git/ref/tags/${TAG}",
        "compare/master...${commit}",
        "check_name=Linux%20%2F%20Required",
        "actions/workflows/nightly.yml/runs?head_sha=${commit}&status=success",
    ):
        assert precondition in workflow
    assert "if: github.event_name == 'push'" in publish_step
    assert "--draft" in publish_step and "--verify-tag" in publish_step
    assert "${{ github.ref_name }}" not in publish_step


def test_deprecation_job_installs_native_build_dependencies() -> None:
    dependency_health = read_workflow("dependency-health.yml")
    deprecation_job = dependency_health.partition("  deprecation-report:\n")[2]
    prerequisite_step = deprecation_job.partition(
        "      - name: Install native build dependencies\n"
    )[2].partition("\n      - name:")[0]

    assert prerequisite_step
    assert "apt-get update" in prerequisite_step
    assert "pkg-config --exists lept tesseract" in prerequisite_step
    for package in DEPRECATION_NATIVE_PACKAGES:
        assert re.search(
            rf"^[ \t]+{re.escape(package)}(?:[ \t]+\\)?[ \t]*$",
            prerequisite_step,
            re.MULTILINE,
        )


def test_zizmor_uses_the_non_vulnerable_pin() -> None:
    ci_manifest = (REPOSITORY_ROOT / "tools" / "ci" / "pyproject.toml").read_text(
        encoding="utf-8"
    )

    assert f'"zizmor=={ZIZMOR_VERSION}",' in ci_manifest
    assert '"zizmor==1.27.0",' not in ci_manifest


def test_rust_toolchain_action_uses_master_pin() -> None:
    expected_reference = (
        f"uses: {RUST_TOOLCHAIN_ACTION}@{RUST_TOOLCHAIN_ACTION_SHA} # master"
    )
    action_references = [
        line.strip()
        for workflow_path in WORKFLOW_DIRECTORY.glob("*.yml")
        for line in workflow_path.read_text(encoding="utf-8").splitlines()
        if f"uses: {RUST_TOOLCHAIN_ACTION}@" in line
    ]

    assert action_references
    assert set(action_references) == {expected_reference}


def test_cuda_validation_stays_out_of_hosted_ci() -> None:
    # A self-hosted runner on a public repository lets fork pull requests run their own
    # workflows there, so the GPU job left CI (#45). The make target stays for GPU hosts.
    makefile = (REPOSITORY_ROOT / "Makefile").read_text(encoding="utf-8")

    assert "cuda-report:" not in read_workflow("nightly.yml")
    assert "report-cuda-integration:" in makefile
    assert "nvidia-smi --query-gpu=" in makefile
    assert "libonnxruntime_providers_cuda.so" in makefile
    assert "DICOM_ANON_REPORT_MODEL_DIR" in makefile
    assert "DICOM_ANON_REPORT_CUDA_ORT_LIBRARY" in makefile
    assert "--no-default-features --test model_cuda" in makefile
    assert "--ignored --test-threads=1" in makefile


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


def test_workflows_do_not_upload_artifacts() -> None:
    workflow_paths = sorted(WORKFLOW_DIRECTORY.glob("*.yml"))

    assert workflow_paths
    for workflow_path in workflow_paths:
        workflow = workflow_path.read_text(encoding="utf-8")
        assert "actions/upload-artifact@" not in workflow, workflow_path.name


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


def test_ci_omits_cross_platform_jobs() -> None:
    workflow_text = "\n".join(
        path.read_text(encoding="utf-8") for path in workflow_paths()
    ).lower()

    for unsupported_runner in ("windows-", "macos-", "arm64"):
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


def test_node_lock_uses_patched_js_yaml() -> None:
    lock = json.loads(
        (REPOSITORY_ROOT / "bindings" / "node" / "package-lock.json").read_text(
            encoding="utf-8"
        )
    )
    js_yaml_version = tuple(
        int(component)
        for component in lock["packages"]["node_modules/js-yaml"]["version"].split(".")
    )

    assert js_yaml_version >= MINIMUM_SAFE_JS_YAML_VERSION


def test_linux_ci_runs_cpu_report_integration_with_pinned_assets() -> None:
    workflow = read_workflow("linux-ci.yml")
    job = workflow.partition("  report-cpu:\n")[2].partition("\n  required:\n")[0]

    assert "name: Linux / Report CPU integration" in job
    assert "run: make report-integration" in job
    assert "model install --backend cpu" in job
    assert (
        "privacy-filter-7ffa9a043d54d1be65afb281eddf0ffbe629385b-ort-1.27.1-cpu" in job
    )
    # Only master runs write the asset cache; everything else only restores it.
    assert "actions/cache/restore@" in job and "actions/cache/save@" in job
    assert "if: github.ref == 'refs/heads/master' &&" in job
    # The Required check also waits for this job.
    assert "REPORT_CPU_RESULT: ${{ needs.report-cpu.result }}" in workflow
