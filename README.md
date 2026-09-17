# mochiOS App Store

This repository contains the native mochiOS App Store client. The current
implementation is an interactive ViewKit mockup backed by an in-memory catalog.

The UI model is intentionally separate from transport and package installation:

```text
AppStore server -> catalog client -> catalog model -> ViewKit UI
                                              |
                                      package service
```

- Server: <https://github.com/mochiOS/AppStore>
- Native client: <https://github.com/mochiOS/AppStore.app>

The mock does not download or install packages. Its GET/Open controls only
exercise presentation state until the authenticated package-service API is
connected.
