# Synthesis Summary

Entry point for downstream consumers (gsd-roadmapper). Synthesized by gsd-doc-synthesizer.

Mode: new (net-new bootstrap)
Precedence: ADR > SPEC > PRD > DOC

## Doc counts by type
- DOC: 1 (RustScreen — Architecture & Implementation Roadmap)
- ADR: 0 | SPEC: 0 | PRD: 0
- Total: 1

The single DOC embeds ADR-like and SPEC-like content; extracted accordingly (see INFO
in INGEST-CONFLICTS.md). Classified DOC at medium confidence — no UNKNOWN/low blocker.

## Cycle detection
- Cross-ref graph acyclic (single in-set node). No traversal-depth or cycle blockers.

## Decisions locked (4 of 8)
- LOCKED (from review): D0 (ports & adapters / "100% Rust source" strategy),
  D1 (USB transport: spike AOA+NCM, lead AOA), D2 (codec: H.264 MVP, HEVC later),
  D3 (Android render: decode-to-surface).
- Proposed defaults: D4 (touch: single-pointer mouse), D5 (Mac app: CLI+menu-bar),
  D6 (geometry: 2400x1080@60 extend), D7 (Android MVP shell: thin Kotlin Activity).
- Source: docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md (§1)
- Detail: .planning/intel/decisions.md

## Requirements extracted (9, P0-P8)
- REQ-p0-workspace-scaffold (COMPLETE — PR #1), REQ-p1-usb-byte-roundtrip,
  REQ-p2-virtual-display-from-rust, REQ-p3-capture-and-encode,
  REQ-p4-decode-and-present, REQ-p5-live-pipeline-and-latency,
  REQ-p6-touch-backchannel-injection, REQ-p7-robustness-ux-purity,
  REQ-p8-packaging-distribution.
- Agreed build order: P0 -> P2 -> P1 (P2 keystone risk front-loaded before P1).
- Detail: .planning/intel/requirements.md

## Constraints extracted (7)
- protocol/platform-boundary: C-ffi-boundaries, C-protocol-wire-format,
  C-target-architecture (3)
- nfr: C-ports-and-adapters (architecture), C-latency-budget (performance),
  C-risk-register (risk, R1-R8) (3)
- schema/project-structure: C-workspace-structure (1)
- Detail: .planning/intel/constraints.md

## Context topics (6)
- Project goal & Definition of Done; tech stack; current status (P0 COMPLETE);
  build order (P2 before P1); phase taxonomy (spike vs build); distribution constraint
  (no MAS, notarized DMG); open questions (latency, entitlements, license, NCM support).
- Detail: .planning/intel/context.md

## Conflicts
- Blockers: 0
- Competing variants: 0
- Auto-resolved: 0
- Single-doc ingest — no cross-document conflicts possible. Three-bucket report
  produced for format compliance.
- Detail: .planning/INGEST-CONFLICTS.md

## Files
- Decisions: .planning/intel/decisions.md
- Requirements: .planning/intel/requirements.md
- Constraints: .planning/intel/constraints.md
- Context: .planning/intel/context.md
- Conflicts: .planning/INGEST-CONFLICTS.md
