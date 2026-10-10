# Internal design notes

These files are historical working notes from the design and early implementation of prx. They
are not user documentation, and parts of them are stale: they describe plans that changed, counts
that have moved, and file names that no longer exist. For how prx works today, read the
[documentation index](../README.md).

They are kept because comments in the source code cite their identifiers:

| Citation in code | Refers to |
|------------------|-----------|
| `A<n>` (for example `A16`) | An audit finding in [design-audit.md](design-audit.md) |
| `DEV-PLAN §<n>` | A section of [DEV-PLAN.md](DEV-PLAN.md), the Rust design contract |
| `spec §<n>` | A section of [spec.md](spec.md), the product specification |
| `design D<n>` and `task <n>.<n>` | A decision or task in an OpenSpec change under `openspec/changes/` (early `D<n>` decisions are in [implementation-plan.md](implementation-plan.md)) |

| File | What it is |
|------|------------|
| [spec.md](spec.md) | Product specification |
| [design-audit.md](design-audit.md) | Design audit findings and UI audit |
| [DEV-PLAN.md](DEV-PLAN.md) | Development plan adapting the spec to Rust |
| [implementation-plan.md](implementation-plan.md) | The original implementation plan, superseded in part |
| [next-steps.plan.md](next-steps.plan.md) | Follow-up phases after the first version |

Do not treat these as current, and do not cite their identifiers in user-facing documentation.
