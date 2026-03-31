#!/usr/bin/env python3
"""Algebraic Coding Decomposition — grokking-friendly training data.

Instead of training on "review this code blob" (natural language, noisy),
we decompose Rust coding into its underlying mathematical operations.
Each is a small, finite, deterministic, rule-based task — exactly what
grokking was designed for.

Task categories:
  1. TYPE_COMPOSE — Result/Option monad composition
  2. MATCH_ARMS — exhaustive pattern match generation
  3. BORROW_CHECK — ownership/borrowing validity
  4. TRAIT_RESOLVE — trait implementation dispatch
  5. TYPE_CHECK — generic type parameter validation
  6. LIFETIME_ORDER — lifetime scope ordering
  7. ITER_TYPE — iterator chain output type inference
  8. FIELD_PROJECT — struct field type projection

Each task has a finite input space and a single deterministic correct answer.

Usage:
    python prepare_algebraic_data.py --output-dir ./data --examples-per-task 500
"""

import argparse
import json
import random
from pathlib import Path

random.seed(42)

SYSTEM_PROMPT = (
    "You are a Rust type system engine. Given a formal type operation, "
    "compute the exact result. Respond with ONLY the answer, no explanation."
)

# ─────────────────────────────────────────────────────────────────
# Primitive types and building blocks
# ─────────────────────────────────────────────────────────────────

PRIMITIVE_TYPES = ["u8", "u16", "u32", "u64", "i32", "i64", "f32", "f64",
                   "bool", "char", "usize", "isize"]
STRING_TYPES = ["String", "&str", "&[u8]", "Vec<u8>"]
COMMON_TYPES = PRIMITIVE_TYPES + STRING_TYPES + ["()", "Vec<String>", "Option<String>"]

ERROR_TYPES = ["IoError", "ParseError", "ConfigError", "NetworkError",
               "TimeoutError", "AuthError", "NotFoundError", "SerdeError"]

TRAITS = ["Display", "Debug", "Clone", "Copy", "Send", "Sync", "Serialize",
          "Deserialize", "Default", "PartialEq", "Eq", "Hash", "Iterator",
          "From", "Into", "AsRef", "TryFrom"]

# Which types implement which traits (simplified but correct)
TRAIT_IMPLS = {
    "u8": ["Display", "Debug", "Clone", "Copy", "Send", "Sync", "Default",
            "PartialEq", "Eq", "Hash", "From", "Into"],
    "u32": ["Display", "Debug", "Clone", "Copy", "Send", "Sync", "Default",
             "PartialEq", "Eq", "Hash", "From", "Into"],
    "u64": ["Display", "Debug", "Clone", "Copy", "Send", "Sync", "Default",
             "PartialEq", "Eq", "Hash"],
    "i32": ["Display", "Debug", "Clone", "Copy", "Send", "Sync", "Default",
             "PartialEq", "Eq", "Hash", "From", "Into"],
    "i64": ["Display", "Debug", "Clone", "Copy", "Send", "Sync", "Default",
             "PartialEq", "Eq", "Hash"],
    "f32": ["Display", "Debug", "Clone", "Copy", "Send", "Sync", "Default", "PartialEq"],
    "f64": ["Display", "Debug", "Clone", "Copy", "Send", "Sync", "Default", "PartialEq"],
    "bool": ["Display", "Debug", "Clone", "Copy", "Send", "Sync", "Default",
              "PartialEq", "Eq", "Hash"],
    "char": ["Display", "Debug", "Clone", "Copy", "Send", "Sync",
              "PartialEq", "Eq", "Hash"],
    "String": ["Display", "Debug", "Clone", "Send", "Sync", "Default",
                "PartialEq", "Eq", "Hash", "From", "AsRef", "Serialize", "Deserialize"],
    "&str": ["Display", "Debug", "Clone", "Copy", "Send", "Sync",
              "PartialEq", "Eq", "Hash"],
    "usize": ["Display", "Debug", "Clone", "Copy", "Send", "Sync", "Default",
               "PartialEq", "Eq", "Hash"],
    "Vec<u8>": ["Debug", "Clone", "Send", "Sync", "Default", "PartialEq", "Eq",
                 "Serialize", "Deserialize"],
    "()": ["Debug", "Clone", "Copy", "Send", "Sync", "Default", "PartialEq", "Eq", "Hash"],
}

ENUM_VARIANTS = {
    "Color": ["Red", "Green", "Blue", "Yellow"],
    "Direction": ["North", "South", "East", "West"],
    "HttpMethod": ["Get", "Post", "Put", "Delete", "Patch"],
    "LogLevel": ["Debug", "Info", "Warn", "Error"],
    "Status": ["Pending", "Running", "Complete", "Failed"],
    "BackendType": ["Ollama", "Openai", "Claude", "Gemini"],
    "ToolTier": ["Safe", "Restricted", "Blocked"],
    "RouterMethod": ["Keyword", "SdrOnly", "LlmConfirmed", "LlmOverride", "Fallback"],
    "Intent": ["Coding", "Reasoning", "HomeAutomation", "Gaming", "Default"],
}

STRUCT_FIELDS = {
    "Config": [("name", "String"), ("port", "u16"), ("enabled", "bool")],
    "Request": [("id", "u64"), ("method", "String"), ("body", "Vec<u8>")],
    "Response": [("status", "u16"), ("headers", "Vec<String>"), ("body", "String")],
    "Session": [("id", "String"), ("user", "String"), ("ttl_secs", "u64")],
    "BackendConfig": [("name", "String"), ("url", "String"), ("max_concurrent", "usize")],
    "ToolSpec": [("name", "String"), ("tier", "String"), ("timeout_secs", "u64")],
}


# ─────────────────────────────────────────────────────────────────
# Task generators
# ─────────────────────────────────────────────────────────────────

def gen_type_compose(n: int) -> list[dict]:
    """Result/Option monad composition — deterministic type algebra."""
    pairs = []
    for _ in range(n):
        t1 = random.choice(PRIMITIVE_TYPES)
        t2 = random.choice(PRIMITIVE_TYPES + STRING_TYPES)
        err = random.choice(ERROR_TYPES)
        op = random.choice(["map", "and_then", "unwrap_or", "ok", "err"])

        if op == "map":
            q = f"TYPE_COMPOSE Result<{t1}, {err}>.map(|x| x as {t2})"
            a = f"Result<{t2}, {err}>"
        elif op == "and_then":
            q = f"TYPE_COMPOSE Result<{t1}, {err}>.and_then(|x| -> Result<{t2}, {err}>)"
            a = f"Result<{t2}, {err}>"
        elif op == "unwrap_or":
            q = f"TYPE_COMPOSE Result<{t1}, {err}>.unwrap_or({t1}::default())"
            a = t1
        elif op == "ok":
            q = f"TYPE_COMPOSE Result<{t1}, {err}>.ok()"
            a = f"Option<{t1}>"
        else:  # err
            q = f"TYPE_COMPOSE Result<{t1}, {err}>.err()"
            a = f"Option<{err}>"

        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": q},
            {"role": "assistant", "content": a},
        ]})

    # Option variants
    for _ in range(n // 2):
        t1 = random.choice(COMMON_TYPES)
        t2 = random.choice(PRIMITIVE_TYPES)
        op = random.choice(["map", "and_then", "unwrap_or", "flatten", "is_some"])

        if op == "map":
            q = f"TYPE_COMPOSE Option<{t1}>.map(|x| -> {t2})"
            a = f"Option<{t2}>"
        elif op == "and_then":
            q = f"TYPE_COMPOSE Option<{t1}>.and_then(|x| -> Option<{t2}>)"
            a = f"Option<{t2}>"
        elif op == "unwrap_or":
            q = f"TYPE_COMPOSE Option<{t1}>.unwrap_or(default)"
            a = t1
        elif op == "flatten":
            q = f"TYPE_COMPOSE Option<Option<{t1}>>.flatten()"
            a = f"Option<{t1}>"
        else:
            q = f"TYPE_COMPOSE Option<{t1}>.is_some()"
            a = "bool"

        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": q},
            {"role": "assistant", "content": a},
        ]})

    return pairs


def gen_match_arms(n: int) -> list[dict]:
    """Exhaustive pattern match — finite enumeration."""
    pairs = []
    for _ in range(n):
        enum_name = random.choice(list(ENUM_VARIANTS.keys()))
        variants = ENUM_VARIANTS[enum_name]
        num_variants = random.randint(2, len(variants))
        selected = variants[:num_variants]

        q = f"MATCH_ARMS enum {enum_name} {{ {', '.join(selected)} }}"
        arms = ", ".join(f"{enum_name}::{v} => _" for v in selected)
        a = f"match x {{ {arms} }}"

        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": q},
            {"role": "assistant", "content": a},
        ]})

    # With data variants
    for _ in range(n // 3):
        q = f"MATCH_ARMS enum Result<{random.choice(PRIMITIVE_TYPES)}, {random.choice(ERROR_TYPES)}>"
        t = random.choice(PRIMITIVE_TYPES)
        e = random.choice(ERROR_TYPES)
        a = f"match x {{ Ok(val) => _, Err(err) => _ }}"
        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": q},
            {"role": "assistant", "content": a},
        ]})

    return pairs


def gen_borrow_check(n: int) -> list[dict]:
    """Ownership/borrowing validity — linear logic rules."""
    pairs = []
    patterns = [
        # Valid borrows
        ("let v: Vec<i32>; let r1 = &v; let r2 = &v; use(r1, r2);",
         "VALID: multiple shared borrows are allowed"),
        ("let mut v: Vec<i32>; let r = &mut v; r.push(1);",
         "VALID: single mutable borrow used exclusively"),
        ("let v: Vec<i32>; let r = &v[0]; drop(r); v.push(1);",
         "VALID: shared borrow dropped before mutable use"),
        ("let s = String::from(\"hello\"); let r = &s; println!(\"{}\", r);",
         "VALID: shared borrow of owned String"),

        # Invalid borrows
        ("let mut v: Vec<i32>; let r = &v[0]; v.push(1); use(r);",
         "ERROR: cannot borrow v as mutable while shared ref r is alive"),
        ("let mut v: Vec<i32>; let r1 = &mut v; let r2 = &mut v;",
         "ERROR: cannot have two mutable borrows of v simultaneously"),
        ("let mut v: Vec<i32>; let r = &v; let m = &mut v;",
         "ERROR: cannot borrow v as mutable while shared ref r exists"),
        ("let s = String::from(\"hello\"); let r = &s; drop(s); use(r);",
         "ERROR: s moved/dropped while shared ref r is alive"),

        # Move semantics
        ("let s1 = String::from(\"hello\"); let s2 = s1; use(s1);",
         "ERROR: use of moved value s1 (String is not Copy)"),
        ("let x: i32 = 5; let y = x; use(x);",
         "VALID: i32 implements Copy, x is still usable"),
        ("let v = vec![1,2,3]; let w = v; use(v);",
         "ERROR: use of moved value v (Vec is not Copy)"),
        ("let v = vec![1,2,3]; let w = v.clone(); use(v);",
         "VALID: v was cloned, original still owned"),
    ]

    for _ in range(n):
        code, result = random.choice(patterns)
        # Slightly randomize types
        for old, new in [(
            "Vec<i32>", f"Vec<{random.choice(PRIMITIVE_TYPES)}>"),
            ("String", random.choice(["String", "Vec<u8>"])),
            ("i32", random.choice(["i32", "u32", "u64", "f64", "bool"])),
        ]:
            if random.random() > 0.5:
                code = code.replace(old, new)
                if "Copy" in result and new in ["Vec<u8>", "String"]:
                    result = result.replace("implements Copy", "is not Copy")
                    if "VALID" in result:
                        result = result.replace("VALID", "ERROR")

        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"BORROW_CHECK {code}"},
            {"role": "assistant", "content": result},
        ]})

    return pairs


def gen_trait_resolve(n: int) -> list[dict]:
    """Trait implementation dispatch — type class resolution."""
    pairs = []
    for _ in range(n):
        ty = random.choice(list(TRAIT_IMPLS.keys()))
        trait = random.choice(TRAITS)
        impls = TRAIT_IMPLS.get(ty, [])
        valid = trait in impls

        q = f"TRAIT_CHECK does {ty} implement {trait}?"
        a = "YES" if valid else "NO"

        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": q},
            {"role": "assistant", "content": a},
        ]})

    # Multi-bound checks
    for _ in range(n // 2):
        ty = random.choice(list(TRAIT_IMPLS.keys()))
        bounds = random.sample(TRAITS, random.randint(2, 3))
        impls = TRAIT_IMPLS.get(ty, [])
        valid = all(b in impls for b in bounds)

        q = f"TRAIT_CHECK does {ty} implement {' + '.join(bounds)}?"
        a = "YES" if valid else f"NO: missing {', '.join(b for b in bounds if b not in impls)}"

        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": q},
            {"role": "assistant", "content": a},
        ]})

    return pairs


def gen_field_project(n: int) -> list[dict]:
    """Struct field type projection — record elimination."""
    pairs = []
    for _ in range(n):
        struct_name = random.choice(list(STRUCT_FIELDS.keys()))
        fields = STRUCT_FIELDS[struct_name]
        field_name, field_type = random.choice(fields)

        q = f"FIELD_TYPE {struct_name}.{field_name}"
        a = field_type

        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": q},
            {"role": "assistant", "content": a},
        ]})

    # Reference projections
    for _ in range(n // 2):
        struct_name = random.choice(list(STRUCT_FIELDS.keys()))
        fields = STRUCT_FIELDS[struct_name]
        field_name, field_type = random.choice(fields)
        ref_type = random.choice(["&", "&mut"])

        q = f"FIELD_TYPE ({ref_type}{struct_name}).{field_name}"
        a = f"{ref_type}{field_type}"

        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": q},
            {"role": "assistant", "content": a},
        ]})

    return pairs


def gen_iter_type(n: int) -> list[dict]:
    """Iterator chain output type inference — function composition."""
    pairs = []
    containers = [
        ("Vec<{T}>", "{T}"),
        ("[{T}; N]", "{T}"),
        ("&[{T}]", "&{T}"),
    ]

    for _ in range(n):
        ty = random.choice(PRIMITIVE_TYPES)
        container, item = random.choice(containers)
        container = container.replace("{T}", ty)
        item = item.replace("{T}", ty)

        ops = random.randint(1, 3)
        chain = f"{container}.iter()"
        result_type = item

        for _ in range(ops):
            op = random.choice(["map", "filter", "cloned", "count", "collect_vec", "sum"])
            if op == "map":
                target = random.choice(PRIMITIVE_TYPES)
                chain += f".map(|x| x as {target})"
                result_type = target
            elif op == "filter":
                chain += ".filter(|x| *x > 0)"
                # type unchanged
            elif op == "cloned":
                chain += ".cloned()"
                result_type = result_type.lstrip("&")
            elif op == "count":
                chain += ".count()"
                result_type = "usize"
                break  # terminal
            elif op == "collect_vec":
                chain += ".collect::<Vec<_>>()"
                result_type = f"Vec<{result_type}>"
                break  # terminal
            elif op == "sum" and result_type in PRIMITIVE_TYPES:
                chain += ".sum::<{result_type}>()"
                # type unchanged
                break

        q = f"ITER_TYPE {chain}"
        a = result_type

        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": q},
            {"role": "assistant", "content": a},
        ]})

    return pairs


# ─────────────────────────────────────────────────────────────────
# Main
# ─────────────────────────────────────────────────────────────────

GENERATORS = {
    "type_compose": gen_type_compose,
    "match_arms": gen_match_arms,
    "borrow_check": gen_borrow_check,
    "trait_resolve": gen_trait_resolve,
    "field_project": gen_field_project,
    "iter_type": gen_iter_type,
}


def main():
    parser = argparse.ArgumentParser(description="Generate algebraic coding data for grokking")
    parser.add_argument("--output-dir", type=Path, default=Path("data"))
    parser.add_argument("--examples-per-task", type=int, default=500,
                        help="Examples per task category")
    parser.add_argument("--task", help="Generate single task only")
    args = parser.parse_args()

    args.output_dir.mkdir(parents=True, exist_ok=True)

    all_examples = []
    for task_name, gen_fn in GENERATORS.items():
        if args.task and args.task != task_name:
            continue
        examples = gen_fn(args.examples_per_task)
        all_examples.extend(examples)
        print(f"  {task_name}: {len(examples)} examples")

    # Shuffle
    random.shuffle(all_examples)

    # Split 50/50 (grokking optimal — enough to learn, enough held out)
    split = len(all_examples) // 2
    train = all_examples[:split]
    val = all_examples[split:]

    train_path = args.output_dir / "algebraic_train.jsonl"
    val_path = args.output_dir / "algebraic_val.jsonl"

    with open(train_path, "w") as f:
        for ex in train:
            f.write(json.dumps(ex) + "\n")

    with open(val_path, "w") as f:
        for ex in val:
            f.write(json.dumps(ex) + "\n")

    print(f"\n{'=' * 50}")
    print(f"Total: {len(all_examples)} examples")
    print(f"  Train: {len(train)} ({train_path})")
    print(f"  Val:   {len(val)} ({val_path})")
    print(f"  Split: 50/50 (grokking optimal)")
    print(f"\nTask distribution:")
    for task_name in GENERATORS:
        count = sum(1 for ex in all_examples
                    if ex["messages"][1]["content"].startswith(task_name.upper().replace("_", " ").split()[0]))
        print(f"  {task_name}: ~{count}")
    print(f"{'=' * 50}")


if __name__ == "__main__":
    main()
