## Conflict Detection Report

Mode: new (net-new bootstrap)
Precedence: ADR > SPEC > PRD > DOC
Docs in ingest set: 1 (single combined architecture + roadmap document)
Cross-ref graph: acyclic (single node; external refs to docs/, README.md, LICENSE,
CONTRIBUTING.md are not in-set and form no cycle).

With exactly one source document, no cross-document conflicts are possible. All three
buckets are empty by construction. The buckets are retained for format compliance.

### BLOCKERS (0)

(none)

Note: The single doc was classified DOC at medium (not low) confidence, so it does NOT
trigger an UNKNOWN-confidence-low blocker. The classifier flagged that the doc embeds
ADR-like locked decisions (D0-D7) and SPEC-like content; these were extracted into
decisions.md and constraints.md respectively. This intra-document precedence promotion
is recorded under INFO below, not as a conflict.

### WARNINGS (0)

(none)

No competing acceptance variants: requirements were derived from a single roadmap's
phases (P0-P8), each with one acceptance criterion. No second source exists to diverge.

### INFO (0 cross-doc conflicts; 2 extraction notes)

[INFO] Intra-document type promotion (not a conflict)
  Source: docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md
  Note: Document is classified DOC, but its §1 decisions table (D0-D7) carries ADR-like
  force and §0/§0.1/§2/§3/§5 carry SPEC-like force. Per the classifier note, D0-D3
  ("locked from review") were extracted as LOCKED decisions; D4-D7 as proposed defaults;
  protocol/architecture/workspace/risk content as constraints. No precedence conflict
  arises because there is only one source.

[INFO] Status + sequencing facts carried into context (not a conflict)
  Source: ingest prompt + roadmap §7
  Note: Phase P0 is recorded COMPLETE (delivered as PR #1) even though its doc checkboxes
  are unticked, and the agreed build order front-loads P2 before P1. Both are recorded in
  context.md for the downstream roadmapper; neither contradicts any other in-set claim.
