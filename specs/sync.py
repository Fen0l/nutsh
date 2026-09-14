#!/usr/bin/env python3
"""Discover and download all Nutanix OpenAPI specs from developers.nutanix.com.

Standalone script — requires only stdlib + httpx (already in the project venv).
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import logging
from datetime import datetime, timezone
from pathlib import Path

import httpx

BASE_URL = "https://developers.nutanix.com/api/v1"
DEFAULT_CONCURRENCY = 5
TIMEOUT = 30.0

log = logging.getLogger("sync")


# ---------------------------------------------------------------------------
# Manifest helpers
# ---------------------------------------------------------------------------


def load_manifest(path: Path) -> dict:
    if path.exists():
        return json.loads(path.read_text())
    return {}


def save_manifest(path: Path, manifest: dict) -> None:
    path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")


# ---------------------------------------------------------------------------
# Discovery
# ---------------------------------------------------------------------------


async def discover_namespaces(client: httpx.AsyncClient) -> list[str]:
    resp = await client.get(f"{BASE_URL}/namespaces")
    resp.raise_for_status()
    return [ns["name"] for ns in resp.json()["namespaces"]]


async def discover_versions(
    client: httpx.AsyncClient,
    namespace: str,
    sem: asyncio.Semaphore,
) -> list[tuple[str, str, str]]:
    """Return list of (namespace, version, yaml_link) triples.

    An announced version with no published spec carries an empty link; a constructed URL 404s.
    """
    async with sem:
        resp = await client.get(f"{BASE_URL}/namespaces/{namespace}/versions")
        resp.raise_for_status()
        return [
            (namespace, v["version"], v.get("link") or "")
            for v in resp.json()["versions"]
        ]


# ---------------------------------------------------------------------------
# Download + change detection
# ---------------------------------------------------------------------------


async def download_spec(
    client: httpx.AsyncClient,
    namespace: str,
    version: str,
    link: str,
    output_dir: Path,
    manifest: dict,
    sem: asyncio.Semaphore,
) -> str:
    """Download a single spec. Returns a status string."""
    key = f"{namespace}/{version}"
    async with sem:
        resp = await client.get(link)
        resp.raise_for_status()

    content = resp.content
    sha = hashlib.sha256(content).hexdigest()

    dest = output_dir / namespace / f"{version}.yaml"
    if key in manifest and manifest[key]["sha256"] == sha and dest.exists():
        log.debug("unchanged  %s", key)
        return "unchanged"

    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_bytes(content)

    manifest[key] = {
        "namespace": namespace,
        "version": version,
        "sha256": sha,
        "downloaded_at": datetime.now(timezone.utc).isoformat(),
    }
    log.info("downloaded %s", key)
    return "downloaded"


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------


async def sync(output_dir: Path, concurrency: int) -> None:
    manifest_path = output_dir / "manifest.json"
    manifest = load_manifest(manifest_path)

    async with httpx.AsyncClient(timeout=TIMEOUT, follow_redirects=True) as client:
        # Phase 1: discover
        log.info("Discovering namespaces...")
        namespaces = await discover_namespaces(client)
        log.info("Found %d namespaces", len(namespaces))

        sem = asyncio.Semaphore(concurrency)
        version_tasks = [discover_versions(client, ns, sem) for ns in namespaces]
        version_results = await asyncio.gather(*version_tasks, return_exceptions=True)

        specs: list[tuple[str, str, str]] = []
        for result in version_results:
            if isinstance(result, Exception):
                log.error("Version discovery failed: %s", result)
                continue
            specs.extend(result)

        # Nothing to download from, so not a failed download.
        published = [s for s in specs if s[2]]
        announced = [(ns, ver) for ns, ver, link in specs if not link]
        for ns, ver in announced:
            log.info("announced with no specification, skipping %s/%s", ns, ver)

        log.info("Found %d specs to check", len(published))

        # Phase 2: download
        download_tasks = [
            download_spec(client, ns, ver, link, output_dir, manifest, sem)
            for ns, ver, link in published
        ]
        results = await asyncio.gather(*download_tasks, return_exceptions=True)

    # Tally results
    counts = {"downloaded": 0, "unchanged": 0, "failed": 0, "skipped": len(announced)}
    for r in results:
        if isinstance(r, Exception):
            log.error("Download failed: %s", r)
            counts["failed"] += 1
        else:
            counts[r] += 1

    save_manifest(manifest_path, manifest)

    log.info(
        "Done: %d downloaded, %d unchanged, %d skipped, %d failed",
        counts["downloaded"],
        counts["unchanged"],
        counts["skipped"],
        counts["failed"],
    )


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Sync Nutanix OpenAPI specs from developers.nutanix.com"
    )
    parser.add_argument("-v", "--verbose", action="store_true", help="Debug logging")
    parser.add_argument(
        "-j",
        "--concurrency",
        type=int,
        default=DEFAULT_CONCURRENCY,
        help=f"Max parallel requests (default: {DEFAULT_CONCURRENCY})",
    )
    parser.add_argument(
        "-o",
        "--output-dir",
        type=Path,
        default=Path(__file__).resolve().parent,
        help="Output directory (default: specs/)",
    )
    args = parser.parse_args()

    logging.basicConfig(
        level=logging.INFO,
        format="%(levelname)-8s %(message)s",
    )
    if args.verbose:
        log.setLevel(logging.DEBUG)
    else:
        logging.getLogger("httpx").setLevel(logging.WARNING)

    asyncio.run(sync(args.output_dir, args.concurrency))


if __name__ == "__main__":
    main()
