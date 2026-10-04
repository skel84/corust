# Incident endpoint contract

Audited against [Coroot cc7c1bf75be9c3d08c751d3d4f9942cf8c81e855](https://github.com/coroot/coroot/tree/cc7c1bf75be9c3d08c751d3d4f9942cf8c81e855): `api/api.go` (`Incidents`, `Incident`), `api/views/incident/incident.go`, `model/application_incident.go`, `model/alert.go`, `timeseries/time.go`, and `db/incident.go`.

The REST list is a `{context, data}` envelope containing an array; an empty rendered list is `[]`. Before a project world is available the handler sends `data: null`. Both return an empty sample. Other shapes, missing identities, missing required impact/duration, malformed evidence and duplicate keys are decode errors. HTTP/context errors keep their existing kinds. The detail handler also sends `data: null` before a world is available; this cannot identify an incident and is a decode error.

List order is open first, then recently opened. The server database limit precedes client app/state filtering. This is a bounded sample, never proof of a complete history or a total count. The list database query is not constrained by the requested display time window. Detail loads the incident's own time context; it is not a normal range-filtered application report.

`impact` is a required number (the maximum availability/latency affected-request percentage); zero is a reported value, not an absent measurement. `duration`, timestamps and burn windows are epoch/duration milliseconds on the wire. Missing/null optional burn values remain `None`. Missing `details` remains no SLO evidence. RCA status, text and propagation issues are source observations, not guarantees of correctness; no analysis is synthesized from healthy/unknown signals.

The detail view optionally carries `availability_slo` and `latency_slo`. Objective and compliance are server-rendered strings. `violated` is a server boolean; latency threshold is seconds. `Project::incident_view` exposes these through additive `IncidentView`/`SloObjective` readers. Existing `Incident`, `Rca`, `Slo`, `BurnRate`, query structs, serialization and method signatures remain usable; `Project::incident` uses the same strict detail validation. This changes previously misleading successful decodes into explicit errors. The old numeric fields are not changed to Options, and callers are not forced to migrate public struct literals.

Charts, RCA widgets, propagation links/healthy nodes and incident mutations are not represented by this client slice. It does not claim ruled-out evidence when only problem propagation applications are decoded.
