## [1.2.2](https://github.com/dlepaux/seerr-mcp/compare/v1.2.1...v1.2.2) (2026-10-19)

### Bug Fixes

* **docker:** label the image with its own description, not the repository's About text ([d312274](https://github.com/dlepaux/seerr-mcp/commit/d312274b3f9db2b8662dba18535fed83b8bf2929))

## 1.2.1 (2026-10-18)

## 1.2.0 (2026-10-18)

### Features

* **search:** retry a miss without its trailing year and say what to try next
* **search:** show each hit's French and English titles, and its original

## 1.1.1 (2026-10-17)

### Bug Fixes

* **seerr:** percent-encode search queries strictly, so multi-word searches work

## 1.1.0 (2026-10-16)

### Features

* ask and answer in the person's language only

## 1.0.3 (2026-10-14)

### Bug Fixes

* **deps:** rustls 0.23.46 for RUSTSEC-2026-0346 to 0349
* **release:** rebuild the image so its OCI version label reads the release, not "latest"

## 1.0.2 (2026-10-13)

### Bug Fixes

* **notify:** post to a per-person Hermes route, ready-<profile>, so two people can be told

## 1.0.1 (2026-10-13)

### Bug Fixes

* **docker:** install the cross target into the pinned toolchain, so the arm64 image builds on x64 runners

## 1.0.0 (2026-10-13)

### Features

* seerr-mcp, so each household member can search and request films in Seerr as themselves
