# Panggung

A lightweight, self-hosted developer portal inspired by Backstage, built with Rust.

Early development: not production-ready or a drop-in Backstage replacement.

## Run locally

Install stable Rust with Cargo and a native linker, then run:

    cd app
    cargo run

Open http://127.0.0.1:3000. The server binds to loopback only. Set PANGGUNG_PORT to select another port and PANGGUNG_DATA_DIR to choose a local data directory.

No separate database server is required. See ROADMAP.md for planned features. The application lives in app/; workspaces/ is a private editable playground excluded from version control.

See CONTRIBUTING.md and SECURITY.md. MIT licensed.