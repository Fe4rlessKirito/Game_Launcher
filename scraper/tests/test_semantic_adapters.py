from __future__ import annotations

from launcher_scraper.adapters import GenericReleaseAdapter
from launcher_scraper.models import SourceDefinition
from launcher_scraper.semantic import SemanticDomBuilder


def test_semantic_dom_is_bounded_and_stable() -> None:
    html = """
    <html><head><title>Example Game 1.2.3</title>
      <meta property="og:title" content="Example Game 1.2.3 Windows">
    </head><body>
      <h1>Example Game 1.2.3</h1>
      <a href="/downloads/game.zip" download="">Download ZIP 4 MB</a>
      <a href="/ads/casino">Click here casino</a>
      <button>More releases</button>
      <form action="/search"><input name="q"></form>
      <span hidden>secret</span><div style="display:none">also secret</div>
    </body></html>
    """
    builder = SemanticDomBuilder(max_text_bytes=100_000)
    page = builder.build(html, "https://example.test/releases/game")
    repeated = builder.build(html, "https://example.test/releases/game")

    assert page.state_hash == repeated.state_hash
    assert "secret" not in page.visible_text
    assert "also secret" not in page.visible_text
    assert page.downloads_detected == ("L1",)
    assert page.links[0].href == "https://example.test/downloads/game.zip"
    assert page.buttons[0].id == "B1"
    assert page.forms[0].fields == ("q",)


def test_generic_adapter_ranks_artifact_links_and_normalizes_release() -> None:
    page = SemanticDomBuilder().build(
        """
        <html><head><title>Example Game v1.2.3 Windows x64</title></head>
        <body><h1>Example Game v1.2.3</h1>
        <a href="/download/game.zip">Download ZIP</a>
        <a href="/ads/casino">Click here casino</a></body></html>
        """,
        "https://example.test/releases/game",
    )
    source = SourceDefinition("example", page.url, gemini_fallback_allowed=False)
    releases = GenericReleaseAdapter().discover(source, page)

    assert len(releases) == 1
    release = releases[0]
    assert release.version == "1.2.3"
    assert release.normalized_product_name == "example game"
    assert release.architecture == "x64"
    assert release.best_download is not None
    assert release.best_download.url.endswith("game.zip")
    assert "casino" not in release.best_download.label.casefold()


def test_generic_adapter_prefers_matching_64_bit_artifact_and_ignores_sidecars() -> None:
    page = SemanticDomBuilder().build(
        """
        <html><head><title>0 A.D. Release 28 Windows</title></head>
        <body>
        <a href="https://releases.example/0ad-0.28.0-win32.exe">Not working? Try the direct download instead.</a>
        <a href="https://releases.example/0ad-0.28.0-win64.exe">Not working? Try the direct download instead.</a>
        <a href="https://releases.example/0ad-0.28.0-win32.exe.torrent">
          32-bit Windows Torrent download (1.518 GB .exe installer)
        </a>
        <a href="https://releases.example/0ad-0.28.0-win64.exe.torrent">
          64-bit Windows Torrent download (1.542 GB .exe installer)
        </a>
        </body></html>
        """,
        "https://example.test/download/win/",
    )
    source = SourceDefinition("0ad", page.url, gemini_fallback_allowed=False)
    adapter = GenericReleaseAdapter()
    release = adapter.discover(source, page)[0]
    candidates = adapter.resolve_downloads(source, release)

    assert release.architecture == "x64"
    assert candidates[0].url.endswith("0ad-0.28.0-win64.exe")
    assert candidates[0].reported_size is None
    assert all(not candidate.url.endswith(".torrent") for candidate in candidates)
