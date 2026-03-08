# Website Backend Documentation

## Architectural Redesign
The centralized backend server has been split from the core network routing engine into the `website_backend` service. 

This specific server is built to exclusively handle:
1. **User Authentication & Authorization (Website Login/Signups).**
2. **Database Migrations and Persistence.**
3. **Billing, User Management, and Bandwidth Cost Analysis.**
4. **Administrative Panels (Region Blocking, DDoS Overviews).**

### File Structure
The project was restructured to adhere to an enterprise-grade modular design to cleanly decouple state configurations from routing definitions.
- `/src/models/` -> Defines the `User` schema mapped to the eventual PostgreSQL pool.
- `/src/db/`     -> Establishes the `sqlx` (Mocked via `RwLock`) Database configuration strings and environment loader.
- `/src/handlers/` -> Business logic routines. Houses the cryptographic implementations (e.g., Argon2).
- `/src/routes/`   -> Maps explicit Axum HTTP routes to their respective handlers conditionally tied to REST methodologies (`POST`/`GET`).

### Cryptography (Authentication)
I utilized the highly secure `argon2` crate to protect user passwords prior to DB injection, utilizing dynamic cryptographic salting via `rand_core::OsRng`.
- **Primary Endpoint:** `POST /api/auth/register` (Parses standard JSON email payloads and cryptographically verifies the generated Hash keys).
