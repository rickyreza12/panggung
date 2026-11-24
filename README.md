# Panggung

A lightweight, self-hosted developer portal inspired by Backstage, built with Rust.

Early development: not production-ready or a drop-in Backstage replacement.

## Run locally

Install stable Rust with Cargo and a native linker, then run:

    cd app
    cargo run -- bootstrap-admin admin
    cargo run

The first command prompts twice for an administrator password without echoing it, creates the initial account once, and exits. Run it interactively in a terminal; do not supply passwords in command arguments. Then open http://127.0.0.1:3000 and sign in. The server binds to loopback only. Set PANGGUNG_PORT to select another port and PANGGUNG_DATA_DIR to choose a local data directory.

No separate database server is required. PANGGUNG_DATABASE_PROVIDER defaults to `sqlite` and PANGGUNG_STORAGE_PROVIDER defaults to `local`; unsupported values fail startup explicitly. Only these provider implementations are available so far.

The local development interface supports authenticated catalog management, teams and memberships, limited Backstage Component descriptor imports from JavaScript Object Notation (JSON) and YAML (YAML Ain't Markup Language), and immutable local object storage. A responsive React and TypeScript frontend ships inside the Rust executable. Local login and role checks are implemented, but **production identity integration, granular resource permissions, backup and restore, horizontal scaling, and the full Backstage ecosystem are not implemented**. See [app/README.md](app/README.md) for the precise supported API subset and [ROADMAP.md](ROADMAP.md) for outstanding milestones.

The application lives in app/; workspaces/ is a private editable playground excluded from version control.

See CONTRIBUTING.md and SECURITY.md. MIT licensed.

## Rebuild frontend assets

The tracked embedded assets let `cargo run` work without a JavaScript toolchain. To regenerate them from source, install Node.js 22 and run from the repository root:

    cd app/frontend
    npm ci --legacy-peer-deps
    npm run typecheck
    npm run lint
    npm run test
    npm run build
    cd ../..
    node app/scripts/sync-frontend.mjs

The continuous integration workflow checks that the embedded assets match the frontend build.
