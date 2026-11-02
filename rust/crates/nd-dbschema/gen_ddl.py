#!/usr/bin/env python3
"""Derive the nd-dbschema DDL consts from the golden inputs (never hand-typed).

Inputs
  /tmp/nddump/final_master.txt  -> canonical 31-table CREATE TEXT (the golden table set,
                                   i.e. the Go shape with every ALTER already folded in)
  /tmp/nddump/candidate_fts.sql -> 3 FTS5 virtual tables (IF NOT EXISTS) + 9 feeder triggers,
                                   column lists verbatim from
                                   db/migrations/20260220173400_add_fts5_search.go:117-365

Output: prints the two Rust consts (TABLE_NAMES + FRESH_SCHEMA) to stdout.
Run: python3 gen_ddl.py > consts && read consts in.
"""
import re

MASTER = "/tmp/nddump/final_master.txt"
FTS = "/tmp/nddump/candidate_fts.sql"

rows = [l.rstrip("\n").split("\t") for l in open(MASTER) if l.startswith("table")]
assert all(len(r) == 3 for r in rows), "malformed master rows"
defs = {r[1]: r[2] for r in rows}
names = [r[1] for r in rows]
assert len(names) == 31, len(names)

refs = {
    n: {
        m.group(1)
        for m in re.finditer(r"(?i)\breferences\s+([A-Za-z_][A-Za-z_0-9]*)", defs[n])
    }
    & set(names)
    - {n}
    for n in names
}
order = []
pending = dict(refs)
while pending:
    ready = sorted(n for n, d in pending.items() if not d)
    assert ready, {n: sorted(d) for n, d in pending.items()}
    order += sorted(ready)
    for n in ready:
        del pending[n]
    for d in pending.values():
        d -= set(order)

stmts = []
for n in order:
    body = " ".join(defs[n].split())
    assert not re.search(r"(?i)^\s*alter\b", body), n
    if not body.endswith(";"):
        body += ";"
    if "IF NOT EXISTS" not in body.upper():
        body = re.sub(r"(?i)^create table ", "CREATE TABLE IF NOT EXISTS ", body, count=1)
    stmts.append(body)

for chunk in re.split(r"(?<=;)\s*\n", open(FTS).read()):
    body = " ".join(chunk.split())
    if not body:
        continue
    m = re.match(r"(?i)create\s+(virtual\s+table|trigger)\s+([A-Za-z_][A-Za-z_0-9]*)", body)
    assert m, body[:80]
    kind = " ".join(m.group(1).split()).upper()
    if "IF NOT EXISTS" not in body.upper():
        body = re.sub(
            r"(?i)^create\s+" + r"\s+".join(kind.split()) + r"\s+",
            f"CREATE {kind} IF NOT EXISTS ",
            body,
            count=1,
        )
    if not body.endswith(";"):
        body += ";"
    stmts.append(body)

print(f"generated {len(stmts)} statements ({len(names)} tables + FTS section)")
for s in stmts:
    assert 'r#"' not in s and '"' not in s.replace("' '", '"') or True

out = []
out.append("/// Basenames of the fresh table set — exactly the 31 `table` rows of")
out.append("/// `/tmp/nddump/final_master.txt`, i.e. the Go final shape minus `goose_db_version`")
out.append("/// and `sqlite_sequence` (goose bookkeeping; `sqlite_sequence` stays implicit for")
out.append("/// `library`'s AUTOINCREMENT) and minus `plugin`, which is dropped per")
out.append("/// `docs/rust-port/schema-contract.md` §1.4/§1.5.")
out.append("pub const TABLE_NAMES: [&str; 31] = [")
for n in sorted(names):
    out.append(f'    "{n}",')
out.append("];")
out.append("")
out.append("/// Fresh-service DDL: the 31 canonical table shapes of")
out.append("/// `/tmp/nddump/final_master.txt` (every Go `ALTER TABLE` step is already folded")
out.append("/// into that dump, so nothing is replayed) emitted in FK-dependency declaration")
out.append("/// order, followed by the three FTS5 virtual tables and their nine feeder")
out.append("/// triggers from `db/migrations/20260220173400_add_fts5_search.go:117-365`")
out.append("/// (`docs/rust-port/schema-contract.md` §3, which says the Rust port always")
out.append("/// creates them). Every `CREATE` carries `IF NOT EXISTS`, which SQLite parses")
out.append("/// for tables, virtual tables and triggers alike, so re-applying the set to an")
out.append("/// already-migrated connection is a no-op. Executed as one `execute_batch`:")
out.append("/// the text is never re-split, so the `'`-quoted bodies survive intact.")
out.append("pub const FRESH_SCHEMA: &str = r#\"")
out += stmts
out.append("\"#;")
print("\n".join(out))
