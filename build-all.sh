#!/usr/bin/env bash
# Build Linux/Windows executables in Docker and a macOS DMG natively.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

usage() {
  echo "Usage: ./build-all.sh [all|linux|windows|macos]"
  echo "Default: all. macOS packaging requires a Mac with Rust and Xcode Command Line Tools."
  echo "Docker builds use the current context; on macOS, Colima starts if Docker is unavailable."
  echo "Outputs: artifacts/linux/, artifacts/windows/, artifacts/macos/"
}

target=${1:-all}
if [[ $# -gt 1 ]]; then usage >&2; exit 2; fi
case "$target" in
  -h|--help) usage; exit 0 ;;
  all|linux|windows|macos) ;;
  *) usage >&2; exit 2 ;;
esac

if [[ "$target" == all || "$target" == macos ]]; then
  [[ $(uname -s) == Darwin ]] || {
    echo "macOS DMG creation requires a Mac. Use 'linux' or 'windows' for Docker-only builds." >&2
    exit 1
  }
fi

if [[ "$target" != macos ]]; then
  if docker compose version >/dev/null 2>&1; then
    compose=(docker compose)
  elif command -v docker-compose >/dev/null 2>&1; then
    compose=(docker-compose)
  else
    echo "Docker with Compose is required. Install Docker Desktop or Docker Engine and Compose." >&2
    exit 1
  fi
  if ! docker info >/dev/null 2>&1; then
    if [[ $(uname -s) == Darwin && -z ${DOCKER_HOST:-} &&
          ( -z ${DOCKER_CONTEXT:-} || ${DOCKER_CONTEXT:-} == colima-openps-build ) ]] &&
          command -v colima >/dev/null; then
      echo "Docker is unavailable; starting Colima's openps-build profile."
      profiles=$(colima list --json)
      if grep -Eq '"name"[[:space:]]*:[[:space:]]*"openps-build"' <<< "$profiles"; then
        colima start openps-build --activate=false
      else
        colima start openps-build --activate=false --cpu 4 --memory 8 --disk 40 --mount "$PWD:w"
      fi
      export DOCKER_CONTEXT=colima-openps-build
    fi
    if ! docker info >/dev/null; then
      echo "Docker is unavailable; start your Docker engine and retry." >&2
      exit 1
    fi
  fi
fi

if [[ "$target" == all || "$target" == linux ]]; then
  "${compose[@]}" run --build --rm linux-build
fi
if [[ "$target" == all || "$target" == windows ]]; then
  "${compose[@]}" run --build --rm windows-build
fi
if [[ "$target" == all || "$target" == macos ]]; then
  ./scripts/make-dmg.sh
fi

echo "Build complete. Outputs: $PWD/artifacts/"
