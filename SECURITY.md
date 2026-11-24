# Security

Panggung is an early-development local tool, not ready for public exposure. Keep it on the default loopback interface (`127.0.0.1`) and do not expose it through a reverse proxy or public interface.

The local build has one-time offline administrator bootstrap, Argon2id password hashes, random session tokens stored as digests in SQLite, expiring/revocable sessions, and basic administrator/member/viewer authorization. Catalog, organization, and object operations require an active local session; viewer writes are rejected. Login attempts share an in-memory rate limit. Mutating browser requests require the exact loopback Origin header, and sessions use host-only, HttpOnly, SameSite=Strict cookies. Loopback Hypertext Transfer Protocol (HTTP) does not provide Transport Layer Security (TLS); this cookie intentionally does not carry the Secure attribute.

These controls have local automated tests, not an independent security audit. There is no external identity federation, fine-grained resource ownership policy, production-grade secrets manager, durable distributed rate limiter, verified backup/restore process, or supported public TLS/reverse-proxy configuration. File permissions on the data directory must restrict it to the application account. Initial administrator passwords are entered interactively, not passed as command-line arguments.

Report vulnerabilities privately to the repository maintainer (use GitHub private vulnerability reporting if enabled). Do not publish credentials, exploit details or private data in public issues.
