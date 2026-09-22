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

Package downloads and installation are not connected yet. The Get control is
disabled until the client can verify a compatible release and hand its MPKG to
`package.service`. The storefront API can list releases whose architecture and
ABI are unknown; those must not be treated as installable.
