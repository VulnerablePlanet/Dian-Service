# Skill Registry

## Compact Rules

### Rust & Architecture
- Follow Hexagonal/Clean Architecture: `core-domain` (no outside IO), adapters for DB (`migrations`, repository implementations), client SOAP (`dian-soap-client`), queue/worker (`worker-redis`), api (`api-server`).
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
