#!/usr/bin/env python3
"""Extract all engrams from Yggdrasil PG into JSONL for Saga training."""

import json
import sys
import psycopg2

BARN = "/data/saga/data"
PG_DSN = "host=<hades-ip> port=5432 dbname=yggdrasil user=yggdrasil password=changeme"

def main():
    conn = psycopg2.connect(PG_DSN)
    cur = conn.cursor()

    cur.execute("""
        SELECT id, cause, effect, tags, trigger_type, trigger_label,
               tier, created_at, access_count
        FROM yggdrasil.engrams
        WHERE cause IS NOT NULL AND effect IS NOT NULL
          AND cause != '' AND effect != ''
          AND NOT ('insight_template' = ANY(tags))
        ORDER BY created_at
    """)

    rows = cur.fetchall()
    cols = [d[0] for d in cur.description]

    out_path = f"{BARN}/engrams_raw.jsonl"
    count = 0
    with open(out_path, "w") as f:
        for row in rows:
            rec = dict(zip(cols, row))
            # Serialize UUID and datetime
            rec["id"] = str(rec["id"])
            rec["created_at"] = rec["created_at"].isoformat() if rec["created_at"] else None
            rec["tags"] = rec["tags"] or []
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
            count += 1

    print(f"Exported {count} engrams to {out_path}")

    # Also export tag distribution for reference
    cur.execute("""
        SELECT tag, COUNT(*) as cnt
        FROM yggdrasil.engrams, LATERAL unnest(tags) AS tag
        WHERE NOT ('insight_template' = ANY(tags))
        GROUP BY tag ORDER BY cnt DESC LIMIT 50
    """)
    tag_dist = cur.fetchall()
    with open(f"{BARN}/tag_distribution.json", "w") as f:
        json.dump({t: c for t, c in tag_dist}, f, indent=2)
    print(f"Tag distribution: {len(tag_dist)} unique tags")

    cur.close()
    conn.close()

if __name__ == "__main__":
    main()
