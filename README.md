# mochiOS App Store

This repository contains the native mochiOS App Store client. The catalog is
loaded from the public `GET /v1/storefront` API. `mmake preview-appstore` uses
the same live endpoint by default; set `APPSTORE_API_BASE_URL` to use a local
API server instead. The preview requires `curl`.

The UI model is intentionally separate from transport and package installation:

```text
AppStore server -> catalog client -> catalog model -> ViewKit UI
                                              |
                                      package service
```

- Server: <https://github.com/mochiOS/AppStore>
- Native client: <https://github.com/mochiOS/AppStore.app>

Selecting a catalog tile opens its detail view. The client then requests the
latest `x86_64` / `mochios-1` release, downloads the MPKG through the public
download endpoint in bounded ranges, verifies the advertised size and SHA-256,
and hands the temporary file to `package.service`. The service remains the
authority for Developer Certificate, manifest, payload, and capability
verification. Temporary packages are removed after success or failure.

The storefront can still contain legacy releases whose architecture and ABI
are unknown. They remain visible but are deliberately not installable; there is
no AdHoc or unfiltered download fallback.
