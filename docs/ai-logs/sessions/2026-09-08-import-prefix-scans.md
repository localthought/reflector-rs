# Import prefix scans

AI assistant: Codex. Human review: the user approved the patch on 2026-09-08 in response to the
request to approve publication and waive separate human testing.

User request: continue fixing atomic-server PR 1383, including a Google Calendar
import that does not complete and imported-drive visibility.

Findings: a stack sample of the running import showed AtomicStorage::put scanning
and decoding the full database when finding obsolete nested resources. Drive
migration also scans the full database for every record.

Changes: use Storelike::resources_with_prefix for migration, nested cleanup,
record reads, collection listing and deletion. The database implementation uses
its key index; other stores retain the existing scan fallback. Update atomic_lib
to the commit providing this API. No authentication or deletion scope changes.

Validation: all 51 library tests pass, including nested object replacement,
namespace-scoped deletion and persistent redb restart tests. The AtomicServer
library adds a prefix-boundary regression test.
