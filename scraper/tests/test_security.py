from __future__ import annotations

import pytest

from launcher_scraper.security import URLPolicy, UrlPolicyError, safe_filename


def test_url_policy_blocks_ssrf_targets() -> None:
    policy = URLPolicy(resolve_dns=False)
    for url in (
        "http://127.0.0.1:8080/",
        "http://10.0.0.4/",
        "http://169.254.169.254/latest/meta-data/",
        "http://user:pass@example.com/file.zip",
        "file:///tmp/file.zip",
    ):
        with pytest.raises(UrlPolicyError):
            policy.validate(url)


def test_localhost_is_only_an_explicit_fixture_exception() -> None:
    policy = URLPolicy(allow_localhost=True, resolve_dns=False)
    assert policy.validate("http://127.0.0.1:8000/release")
    with pytest.raises(UrlPolicyError):
        policy.validate("http://169.254.169.254/latest/meta-data/")


def test_safe_filename_removes_paths_and_control_characters() -> None:
    assert safe_filename("../../evil<>:\\artifact.zip") == "artifact.zip"
    assert safe_filename("   ") == "download.bin"
    assert len(safe_filename("a" * 400 + ".zip")) <= 180
