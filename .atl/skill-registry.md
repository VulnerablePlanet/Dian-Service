# Skill Registry

## Compact Rules

### Rust & Architecture
- Follow Hexagonal/Clean Architecture: `core-domain` (pure: models, rules and ports; no sqlx/HTTP), use cases in `application`, adapters for DB and job queue (`infra-postgres`, `migrations`), SOAP client (`dian-soap-client`), signing (`crypto-signer`), and thin binaries (`api-server`, `worker`).
- State changes go through `DocumentRepository::apply_transition` (conditional on the current status, atomic with event and job outcome). Never update `documents.status` directly.
- Database: Keep DB transactions short. DO NOT invoke external SOAP endpoints (DIAN) inside an active database transaction.
- UBL & Cryptography: Sign UBL XML documents with XAdES-EPES. Calculate CUFE/CUDE with proper SHA-384.

### SDD & TDD
- Strict TDD Mode is enabled. Write unit tests for new domain rules or UBL helpers.
- Run `cargo test` to verify.

## User Skills
- sdd-explore: Explore changes
- sdd-propose: Propose architectural change
- sdd-spec: Write scenarios and specifications
- sdd-design: Code design decisions
- sdd-tasks: Checklists
- sdd-apply: Implement code
- sdd-verify: Run validation
- sdd-archive: Archive changes
