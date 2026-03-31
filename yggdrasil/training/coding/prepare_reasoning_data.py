#!/usr/bin/env python3
"""Computational Reasoning Primitives — 5 Pillars of Code Thinking.

Generates training data for the fundamental reasoning skills that underlie
ALL programming, language-agnostic. Each task has deterministic correct answers
derived from computational theory, not language-specific syntax.

Pillars:
  1. DECOMPOSE — Break goals into ordered atomic operations
  2. AST — Reason about abstract syntax tree structure
  3. DATA_STRUCTURE — Select optimal data structure for access pattern
  4. CONTROL_FLOW — Determine correct flow pattern for a task
  5. DEBUG — Given error + code, identify root cause and fix

Each pillar generates small, deterministic, rule-based tasks — ideal for grokking.
"""

import argparse
import json
import random
from pathlib import Path

random.seed(42)

SYSTEM_PROMPT = (
    "You are a computational reasoning engine. Analyze the problem using "
    "fundamental computer science principles. Respond with ONLY the answer "
    "in the exact format requested."
)


# ─────────────────────────────────────────────────────────────────
# Pillar 1: DECOMPOSE — Problem Decomposition & Ordering
# ─────────────────────────────────────────────────────────────────

DECOMPOSE_TASKS = [
    # (goal, dependencies as edges, correct topological order)
    {
        "goal": "Deploy a web service",
        "steps": ["write_code", "run_tests", "build_binary", "deploy_to_server", "verify_health"],
        "deps": [("run_tests", "write_code"), ("build_binary", "run_tests"),
                 ("deploy_to_server", "build_binary"), ("verify_health", "deploy_to_server")],
        "order": "write_code -> run_tests -> build_binary -> deploy_to_server -> verify_health",
    },
    {
        "goal": "Process user authentication",
        "steps": ["receive_request", "validate_input", "hash_password", "query_database", "check_match", "generate_token", "return_response"],
        "deps": [("validate_input", "receive_request"), ("hash_password", "validate_input"),
                 ("query_database", "validate_input"), ("check_match", "hash_password"),
                 ("check_match", "query_database"), ("generate_token", "check_match"),
                 ("return_response", "generate_token")],
        "order": "receive_request -> validate_input -> [hash_password, query_database] -> check_match -> generate_token -> return_response",
    },
    {
        "goal": "Parse and transform a config file",
        "steps": ["read_file", "validate_format", "parse_fields", "expand_env_vars", "validate_values", "return_config"],
        "deps": [("validate_format", "read_file"), ("parse_fields", "validate_format"),
                 ("expand_env_vars", "parse_fields"), ("validate_values", "expand_env_vars"),
                 ("return_config", "validate_values")],
        "order": "read_file -> validate_format -> parse_fields -> expand_env_vars -> validate_values -> return_config",
    },
    {
        "goal": "Implement a search feature",
        "steps": ["receive_query", "tokenize_query", "search_index", "rank_results", "filter_by_permissions", "paginate", "return_results"],
        "deps": [("tokenize_query", "receive_query"), ("search_index", "tokenize_query"),
                 ("rank_results", "search_index"), ("filter_by_permissions", "rank_results"),
                 ("paginate", "filter_by_permissions"), ("return_results", "paginate")],
        "order": "receive_query -> tokenize_query -> search_index -> rank_results -> filter_by_permissions -> paginate -> return_results",
    },
    {
        "goal": "Handle file upload",
        "steps": ["receive_stream", "validate_size", "validate_type", "write_temp", "scan_malware", "move_to_storage", "update_database", "return_url"],
        "deps": [("validate_size", "receive_stream"), ("validate_type", "validate_size"),
                 ("write_temp", "validate_type"), ("scan_malware", "write_temp"),
                 ("move_to_storage", "scan_malware"), ("update_database", "move_to_storage"),
                 ("return_url", "update_database")],
        "order": "receive_stream -> validate_size -> validate_type -> write_temp -> scan_malware -> move_to_storage -> update_database -> return_url",
    },
]


def gen_decompose(n: int) -> list[dict]:
    """Generate problem decomposition tasks."""
    pairs = []

    for _ in range(n):
        task = random.choice(DECOMPOSE_TASKS)

        # Forward: goal → steps in order
        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"DECOMPOSE {task['goal']}\nAvailable steps: {', '.join(task['steps'])}"},
            {"role": "assistant", "content": task["order"]},
        ]})

        # Dependency check: given two steps, which must come first?
        dep = random.choice(task["deps"])
        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"DEPENDENCY In '{task['goal']}': which must complete first, {dep[0]} or {dep[1]}?"},
            {"role": "assistant", "content": f"{dep[1]} must complete before {dep[0]}"},
        ]})

        # Parallel detection: which steps can run concurrently?
        if any("parallel" in task["order"].lower() or "[" in task["order"] for _ in [0]):
            pass  # Tasks with parallel steps already encoded

    return pairs


# ─────────────────────────────────────────────────────────────────
# Pillar 2: AST — Abstract Syntax Tree Reasoning
# ─────────────────────────────────────────────────────────────────

AST_NODES = {
    "assignment": {"children": ["target", "value"], "example": "x = expr"},
    "if_else": {"children": ["condition", "then_branch", "else_branch"], "example": "if cond then A else B"},
    "while_loop": {"children": ["condition", "body"], "example": "while cond do body"},
    "for_loop": {"children": ["iterator", "iterable", "body"], "example": "for i in collection do body"},
    "function_def": {"children": ["name", "params", "return_type", "body"], "example": "fn name(params) -> ret { body }"},
    "function_call": {"children": ["callee", "arguments"], "example": "callee(args)"},
    "binary_op": {"children": ["left", "operator", "right"], "example": "left op right"},
    "return_stmt": {"children": ["value"], "example": "return value"},
    "match_expr": {"children": ["scrutinee", "arms"], "example": "match x { arms }"},
    "struct_def": {"children": ["name", "fields"], "example": "struct Name { fields }"},
    "field_access": {"children": ["object", "field"], "example": "object.field"},
    "method_call": {"children": ["receiver", "method", "arguments"], "example": "receiver.method(args)"},
    "let_binding": {"children": ["pattern", "type_annotation", "initializer"], "example": "let pat: Type = init"},
    "index_access": {"children": ["collection", "index"], "example": "collection[index]"},
    "block": {"children": ["statements", "final_expression"], "example": "{ stmts; expr }"},
}


def gen_ast(n: int) -> list[dict]:
    """Generate AST reasoning tasks."""
    pairs = []

    for _ in range(n):
        node_type = random.choice(list(AST_NODES.keys()))
        node = AST_NODES[node_type]

        # Task 1: identify node type from structure
        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"AST_NODE What AST node type has children: {', '.join(node['children'])}?"},
            {"role": "assistant", "content": node_type},
        ]})

        # Task 2: list children of a node type
        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"AST_CHILDREN What are the child nodes of a {node_type} AST node?"},
            {"role": "assistant", "content": ", ".join(node["children"])},
        ]})

        # Task 3: identify node type from code pattern
        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"AST_CLASSIFY What AST node type is: {node['example']}"},
            {"role": "assistant", "content": node_type},
        ]})

        # Task 4: depth calculation for nested expressions
        depth = random.randint(1, 5)
        if node_type == "binary_op":
            # Build nested binary op: ((a + b) * c) - d
            ops = ["+", "-", "*", "/", "&&", "||", "==", "!=", "<", ">"]
            expr = "x"
            for d in range(depth):
                op = random.choice(ops)
                expr = f"({expr} {op} y{d})"
            pairs.append({"messages": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": f"AST_DEPTH What is the nesting depth of: {expr}"},
                {"role": "assistant", "content": str(depth)},
            ]})

    return pairs


# ─────────────────────────────────────────────────────────────────
# Pillar 3: DATA_STRUCTURE — Selection by Access Pattern
# ─────────────────────────────────────────────────────────────────

DS_RULES = [
    # (access_pattern, correct_structure, reasoning)
    ("need O(1) lookup by key", "HashMap", "key-value lookup in constant time"),
    ("need O(1) lookup by unique string key", "HashMap<String, V>", "string-keyed constant time lookup"),
    ("need ordered iteration", "Vec", "contiguous memory, cache-friendly iteration"),
    ("need ordered iteration with fast append", "Vec", "amortized O(1) push, ordered traversal"),
    ("need FIFO queue", "VecDeque", "O(1) push_back and pop_front"),
    ("need LIFO stack", "Vec", "O(1) push and pop from end"),
    ("need sorted unique elements", "BTreeSet", "sorted order with O(log n) operations"),
    ("need fast membership test", "HashSet", "O(1) contains check"),
    ("need priority ordering", "BinaryHeap", "O(log n) insert, O(1) peek max"),
    ("need key-value with sorted keys", "BTreeMap", "sorted key iteration with O(log n) lookup"),
    ("need bidirectional linked structure", "LinkedList", "O(1) insert/remove at both ends"),
    ("need graph with weighted edges", "HashMap<Node, Vec<(Node, Weight)>>", "adjacency list representation"),
    ("need fixed-size buffer with wrap-around", "VecDeque with capacity", "ring buffer semantics"),
    ("need concurrent read-write access", "RwLock<HashMap>", "multiple readers or single writer"),
    ("need atomic counter", "AtomicU64", "lock-free increment/decrement"),
    ("need thread-safe queue", "Mutex<VecDeque>", "protected FIFO across threads"),
    ("need to deduplicate while preserving order", "IndexSet", "insertion-ordered unique elements"),
    ("need sparse array with default values", "HashMap<usize, V>", "only store non-default entries"),
    ("need string interning", "HashSet<String> with indices", "deduplicate strings, reference by index"),
    ("need LRU cache", "LinkedHashMap or custom with HashMap + LinkedList", "O(1) access with eviction of least recently used"),
]


def gen_data_structure(n: int) -> list[dict]:
    """Generate data structure selection tasks."""
    pairs = []

    for _ in range(n):
        rule = random.choice(DS_RULES)

        # Forward: pattern → structure
        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"DATA_STRUCTURE I {rule[0]}. What should I use?"},
            {"role": "assistant", "content": f"{rule[1]} — {rule[2]}"},
        ]})

        # Reverse: given structure, what's it good for?
        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"DATA_STRUCTURE_USE When would I use {rule[1]}?"},
            {"role": "assistant", "content": f"When you {rule[0]}"},
        ]})

        # Complexity: what's the time complexity?
        if "O(1)" in rule[0]:
            complexity = "O(1)"
        elif "O(log n)" in rule[2]:
            complexity = "O(log n)"
        elif "sorted" in rule[0]:
            complexity = "O(log n)"
        else:
            complexity = "O(1) amortized"

        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"COMPLEXITY What is the primary operation complexity of {rule[1]}?"},
            {"role": "assistant", "content": complexity},
        ]})

    return pairs


# ─────────────────────────────────────────────────────────────────
# Pillar 4: CONTROL_FLOW — Pattern Selection
# ─────────────────────────────────────────────────────────────────

FLOW_PATTERNS = [
    # (task_description, correct_pattern, pseudocode)
    ("process each item in a collection", "for_each",
     "for item in collection { process(item) }"),
    ("repeat until a condition is met", "while_loop",
     "while !condition { do_work(); update_condition(); }"),
    ("try operation, retry N times on failure", "retry_loop",
     "for attempt in 1..=N { match try_op() { Ok(v) => return v, Err(e) if attempt < N => sleep(backoff), Err(e) => return Err(e) } }"),
    ("execute different logic based on a value", "match_expr",
     "match value { Pattern1 => action1(), Pattern2 => action2(), _ => default() }"),
    ("transform a collection into a new one", "iterator_map",
     "collection.iter().map(|x| transform(x)).collect()"),
    ("find first element matching a predicate", "iterator_find",
     "collection.iter().find(|x| predicate(x))"),
    ("aggregate a collection into a single value", "fold_reduce",
     "collection.iter().fold(initial, |acc, x| combine(acc, x))"),
    ("run two independent operations concurrently", "async_join",
     "let (a, b) = tokio::join!(op_a(), op_b());"),
    ("run operation with timeout", "async_timeout",
     "match tokio::time::timeout(duration, operation()).await { Ok(result) => result, Err(_) => handle_timeout() }"),
    ("process a stream of events indefinitely", "event_loop",
     "loop { match receiver.recv().await { Some(event) => handle(event), None => break } }"),
    ("guard a critical section", "mutex_lock",
     "let guard = mutex.lock().await; modify_shared_state(&mut guard); drop(guard);"),
    ("perform cleanup regardless of success/failure", "scope_guard",
     "let _guard = scopeguard::guard(resource, |r| cleanup(r)); do_work();"),
    ("traverse a tree structure", "recursion",
     "fn traverse(node) { process(node); for child in node.children { traverse(child) } }"),
    ("partition items into groups", "group_by",
     "let groups: HashMap<Key, Vec<Item>> = items.into_iter().fold(HashMap::new(), |mut m, item| { m.entry(key(&item)).or_default().push(item); m })"),
    ("chain multiple fallible operations", "question_mark",
     "let a = step1()?; let b = step2(a)?; let c = step3(b)?; Ok(c)"),
    ("select first successful result from alternatives", "try_alternatives",
     "op_a().or_else(|| op_b()).or_else(|| op_c()).ok_or(Error::AllFailed)"),
]


def gen_control_flow(n: int) -> list[dict]:
    """Generate control flow pattern selection tasks."""
    pairs = []

    for _ in range(n):
        pattern = random.choice(FLOW_PATTERNS)

        # Forward: task → pattern
        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"CONTROL_FLOW I need to: {pattern[0]}. What pattern should I use?"},
            {"role": "assistant", "content": f"{pattern[1]}: {pattern[2]}"},
        ]})

        # Reverse: pattern → when to use
        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"CONTROL_FLOW_USE When would I use the {pattern[1]} pattern?"},
            {"role": "assistant", "content": f"When you need to {pattern[0]}"},
        ]})

    return pairs


# ─────────────────────────────────────────────────────────────────
# Pillar 5: DEBUG — Error → Root Cause → Fix
# ─────────────────────────────────────────────────────────────────

DEBUG_PATTERNS = [
    # (error_message, code_context, root_cause, fix)
    ("index out of bounds: len is 5 but index is 5",
     "let x = vec[vec.len()];",
     "Off-by-one: array indices are 0-based, so max valid index is len-1",
     "let x = vec[vec.len() - 1]; // or use .last()"),

    ("cannot borrow as mutable because it is also borrowed as immutable",
     "let r = &data[0]; data.push(1); println!(\"{}\", r);",
     "Shared borrow r is still alive when mutable borrow (push) occurs",
     "Drop the shared borrow before mutating: let val = data[0]; data.push(1);"),

    ("type mismatch: expected u32, found i32",
     "let x: u32 = some_i32_value;",
     "Implicit integer type coercion is not allowed in Rust",
     "let x: u32 = some_i32_value as u32; // or use try_into() for checked conversion"),

    ("thread 'main' panicked at 'called unwrap() on a None value'",
     "let val = map.get(key).unwrap();",
     "The key does not exist in the map, get() returns None",
     "let val = map.get(key).unwrap_or(&default); // or use if let Some(v) = map.get(key)"),

    ("deadlock detected: two locks acquired in different order",
     "lock_a.lock(); lock_b.lock(); // thread 1\nlock_b.lock(); lock_a.lock(); // thread 2",
     "Lock ordering inconsistency causes circular wait",
     "Always acquire locks in the same order: lock_a then lock_b in both threads"),

    ("stack overflow: recursive function without base case",
     "fn factorial(n: u64) -> u64 { n * factorial(n - 1) }",
     "No base case: recursion never terminates when n reaches 0",
     "fn factorial(n: u64) -> u64 { if n <= 1 { 1 } else { n * factorial(n - 1) } }"),

    ("connection refused: tcp connect error on port 8080",
     "let resp = client.get(url).send().await?;",
     "Target service is not running or not listening on the expected port",
     "Add retry with backoff, verify service is running, check port configuration"),

    ("value used after move",
     "let s = String::from(\"hello\"); let t = s; println!(\"{}\", s);",
     "String is not Copy — assignment moves ownership to t, s is invalid",
     "Use s.clone() if you need both, or restructure to avoid the move"),

    ("mismatched types: expected &str, found String",
     "fn greet(name: &str) {} greet(my_string);",
     "Function expects a borrowed string slice but received an owned String",
     "greet(&my_string) — borrow the String as &str"),

    ("integer overflow: attempt to add with overflow",
     "let x: u8 = 255; let y = x + 1;",
     "u8 max value is 255, adding 1 causes overflow (panics in debug, wraps in release)",
     "Use checked_add: x.checked_add(1).unwrap_or(u8::MAX) or use a larger type"),

    ("cannot move out of borrowed content",
     "fn take(v: &Vec<String>) -> String { v[0] }",
     "Indexing a borrowed Vec tries to move the element out, but the Vec is borrowed",
     "Return a clone: v[0].clone() or return a reference: &v[0]"),

    ("lifetime mismatch: borrowed value does not live long enough",
     "fn longest(a: &str, b: &str) -> &str { if a.len() > b.len() { a } else { b } }",
     "Return type has no lifetime annotation — compiler can't determine which input's lifetime applies",
     "fn longest<'a>(a: &'a str, b: &'a str) -> &'a str { ... }"),
]


def gen_debug(n: int) -> list[dict]:
    """Generate debug reasoning tasks."""
    pairs = []

    for _ in range(n):
        pattern = random.choice(DEBUG_PATTERNS)

        # Error → root cause
        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"DEBUG_CAUSE Error: {pattern[0]}\nCode: {pattern[1]}"},
            {"role": "assistant", "content": pattern[2]},
        ]})

        # Error → fix
        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"DEBUG_FIX Error: {pattern[0]}\nCode: {pattern[1]}"},
            {"role": "assistant", "content": pattern[3]},
        ]})

        # Code → predict error (reverse)
        pairs.append({"messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"DEBUG_PREDICT What error will this code produce?\n{pattern[1]}"},
            {"role": "assistant", "content": pattern[0]},
        ]})

    return pairs


# ─────────────────────────────────────────────────────────────────
# Main
# ─────────────────────────────────────────────────────────────────

GENERATORS = {
    "decompose": gen_decompose,
    "ast": gen_ast,
    "data_structure": gen_data_structure,
    "control_flow": gen_control_flow,
    "debug": gen_debug,
}


def main():
    parser = argparse.ArgumentParser(description="Generate 5-pillar reasoning data for grokking")
    parser.add_argument("--output-dir", type=Path, default=Path("data"))
    parser.add_argument("--examples-per-pillar", type=int, default=500)
    args = parser.parse_args()

    args.output_dir.mkdir(parents=True, exist_ok=True)

    all_examples = []
    for name, gen_fn in GENERATORS.items():
        examples = gen_fn(args.examples_per_pillar)
        all_examples.extend(examples)
        print(f"  {name}: {len(examples)} examples")

    random.shuffle(all_examples)

    # 50/50 split for grokking
    split = len(all_examples) // 2
    train = all_examples[:split]
    val = all_examples[split:]

    train_path = args.output_dir / "reasoning_train.jsonl"
    val_path = args.output_dir / "reasoning_val.jsonl"

    with open(train_path, "w") as f:
        for ex in train:
            f.write(json.dumps(ex) + "\n")
    with open(val_path, "w") as f:
        for ex in val:
            f.write(json.dumps(ex) + "\n")

    print(f"\n{'=' * 50}")
    print(f"Total: {len(all_examples)} reasoning examples")
    print(f"  Train: {len(train)} ({train_path})")
    print(f"  Val:   {len(val)} ({val_path})")
    print(f"{'=' * 50}")


if __name__ == "__main__":
    main()
