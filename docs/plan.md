## Goal

The goal is a stateless mcp server for CalDAV that accepts it's config through env vars at start.

Example using an app template from my k8s homelab:

```yaml
apiVersion: helm.toolkit.fluxcd.io/v2
kind: HelmRelease
metadata:
  name: calmcp
  namespace: home 
spec:
  interval: 1h

  chart:
    spec:
      chart: app-template
      sourceRef:
        kind: HelmRepository
        name: bjw-s-labs
        namespace: flux-system

  install:
    createNamespace: true

  values:
    controllers:
      romm:
        containers:
          app:
            image:
              repository: rkhalil38/calmcp
              tag: latest@sha256:8677fb7b81b1d3db20b849e989a1dadc2a11159e51eb34a2acdcb01102274edf
            env:
              TZ: ${TZ}
              PORT: &port 8080
              CALDAV_URL: http://radicale.home:5232
              CALDAV_USERNAME: romulus
              MCP_API_KEY: whoosh

            envFrom:
              - secretRef:
                  name: radicale-login-secret

    service:
      app:
        controller: calmcp 
        ports:
          http:
            port: *port
```

### MCP

MCP will be done through the official [rust-sdk](https://github.com/modelcontextprotocol/rust-sdk).

### CalDAV

CalDAV operations will be done through [libdav](https://docs.rs/libdav/latest/libdav/).
