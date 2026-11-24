# Panggung Agent Rules

## Repository boundaries
- `app/` contains the real, deployable Rust application.
- `workspaces/` is the owner's editable playground for prompts, designs, experiments and reports.
- Never modify app/ for a workspace-only task. Never execute a prompt merely because it was written.
- Read the root and relevant scoped AGENTS.md before acting. Preserve user edits.

## Required workflow
- Use caveman for concise reports, ponytail for the smallest complete change, and clean-code before inspecting or editing code.
- Use Rust Token Killer (RTK) for supported searches, file inspection, Git diffs and test output. Read its help first; never guess commands or install without permission.
- If skills or RTK are unavailable, use ordinary tools and apply equivalent principles.
- Inspect the task, relevant files, callers, tests, configuration and Git state before editing.
- Work until verified completion or a documented external blocker.

## Engineering principles
- Keep It Simple, Stupid (KISS): prefer straightforward designs.
- You Aren't Gonna Need It (YAGNI): avoid speculative features and dependencies.
- Don't Repeat Yourself (DRY): extract abstractions after real repetition.
- Apply SOLID pragmatically: Single Responsibility, Open/Closed, Liskov Substitution, Interface Segregation and Dependency Inversion.
- Prefer reuse and deletion over new layers; fix root causes and affected callers.
- Validate trust boundaries, preserve meaningful errors and add focused tests.
- Never weaken tests or validation to obtain a pass.

## Security and Git
- Never expose secrets, tokens, cookies, connection strings or private operational data.
- Keep credentials out of source control, browser bundles and logs.
- Use clearly labeled synthetic demo data; never fabricate operational results.
- Never reset or broadly delete databases without explicit approval.
- Inspect branch, status, remotes, upstream and relevant diffs before Git changes.
- Preserve unfamiliar or uncommitted user work.
- Never force push, rewrite history, hard reset or blindly resolve conflicts.
- Stage only reviewed task files; use caveman-commit when available.

## Verification
- Run focused checks, then relevant tests and builds.
- Report actual results, changed paths, limitations and blockers concisely.
- Never claim unexecuted checks succeeded.
