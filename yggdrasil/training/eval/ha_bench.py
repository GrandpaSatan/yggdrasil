#!/usr/bin/env python3
"""Yggdrasil HA Automation Benchmark.

Tests LLM ability to generate valid Home Assistant automation YAML given
entity lists and descriptions.  Mirrors the AutomationGenerator pattern
in ygg-ha/src/automation.rs.

Scoring:
  - YAML validity (parseable)
  - Required sections present (trigger, action, alias, mode)
  - Uses only provided entity IDs
  - Correct platform/service usage

Usage:
    python ha_bench.py --model LFM2-24B-A2B --ollama-url http://localhost:11434
"""

import argparse
import json
import re
import time
from dataclasses import dataclass, asdict
from pathlib import Path

import requests

try:
    import yaml
    HAS_YAML = True
except ImportError:
    HAS_YAML = False

SYSTEM_PROMPT = """You are a Home Assistant automation generator for Yggdrasil.
Given a description and available entities, generate valid Home Assistant automation YAML.
Rules:
- Output ONLY valid YAML inside a ```yaml code fence
- Use only the provided entity IDs
- Include: alias, mode (single), trigger, action sections
- Use correct HA platform names (state, time, sun, numeric_state)
- Use correct HA service call format (service: domain.action)
"""

ENTITIES = {
    "lights": [
        "light.living_room", "light.bedroom", "light.kitchen",
        "light.office", "light.garage",
    ],
    "switches": [
        "switch.fan_living_room", "switch.heater_bedroom",
    ],
    "sensors": [
        "sensor.temperature_living_room", "sensor.humidity_bedroom",
        "sensor.motion_hallway", "sensor.door_front",
    ],
    "climate": ["climate.hvac_main"],
    "media_player": ["media_player.living_room_tv"],
}

ENTITY_IDS = [eid for group in ENTITIES.values() for eid in group]

TASKS = [
    {
        "id": "sunset_lights",
        "name": "Sunset Light Automation",
        "description": "Turn on the living room and kitchen lights at sunset, "
                       "and turn them off at 11:30 PM.",
        "required_entities": ["light.living_room", "light.kitchen"],
        "required_triggers": ["sun"],
        "required_services": ["light.turn_on"],
    },
    {
        "id": "temperature_hvac",
        "name": "Temperature-Triggered HVAC",
        "description": "When the living room temperature drops below 18°C, "
                       "turn on the HVAC in heat mode. When it rises above 24°C, "
                       "switch to cool mode.",
        "required_entities": ["sensor.temperature_living_room", "climate.hvac_main"],
        "required_triggers": ["numeric_state"],
        "required_services": ["climate.set_hvac_mode"],
    },
    {
        "id": "motion_notification",
        "name": "Motion Sensor Notification",
        "description": "When motion is detected in the hallway and the front door "
                       "sensor shows 'open', send a mobile notification saying "
                       "'Motion detected with door open'.",
        "required_entities": ["sensor.motion_hallway", "sensor.door_front"],
        "required_triggers": ["state"],
        "required_services": ["notify"],
    },
    {
        "id": "multi_condition",
        "name": "Multi-Condition Automation",
        "description": "Every weekday at 7:00 AM, if the bedroom humidity is below 40%, "
                       "turn on the bedroom heater switch for 30 minutes.",
        "required_entities": ["sensor.humidity_bedroom", "switch.heater_bedroom"],
        "required_triggers": ["time"],
        "required_services": ["switch.turn_on"],
    },
]


@dataclass
class HaResult:
    task_id: str
    task_name: str
    model: str
    yaml_valid: bool = False
    has_alias: bool = False
    has_mode: bool = False
    has_trigger: bool = False
    has_action: bool = False
    uses_correct_entities: bool = False
    has_required_trigger: bool = False
    has_required_service: bool = False
    score: float = 0.0
    latency_ms: int = 0
    tok_per_sec: float = 0.0
    response: str = ""
    error: str = ""


def extract_yaml_block(text: str) -> str:
    """Extract YAML from code fence or raw text."""
    match = re.search(r"```ya?ml\s*\n([\s\S]*?)```", text)
    if match:
        return match.group(1).strip()
    # Try raw YAML (starts with key:)
    lines = text.strip().split("\n")
    yaml_lines = []
    started = False
    for line in lines:
        if re.match(r"^\w[\w_]*:", line):
            started = True
        if started:
            yaml_lines.append(line)
    return "\n".join(yaml_lines) if yaml_lines else text.strip()


def validate_yaml(text: str) -> tuple[bool, dict | list | None]:
    """Check if text is valid YAML."""
    if not HAS_YAML:
        # Fallback: check for key structural markers
        has_keys = bool(re.search(r"^\w+:", text, re.MULTILINE))
        return has_keys, None
    try:
        data = yaml.safe_load(text)
        return data is not None, data
    except yaml.YAMLError:
        return False, None


def check_entities_used(yaml_text: str, required: list[str], all_valid: list[str]) -> bool:
    """Check if required entities are mentioned and no invalid ones are used."""
    for eid in required:
        if eid not in yaml_text:
            return False
    # Check for entity-like strings that aren't in our valid set
    found = re.findall(r"\b((?:light|switch|sensor|climate|media_player)\.\w+)\b", yaml_text)
    for eid in found:
        if eid not in all_valid:
            return False
    return True


def score_task(task: dict, yaml_text: str, parsed: dict | list | None) -> HaResult:
    """Score a YAML automation response."""
    result = HaResult(task_id=task["id"], task_name=task["name"], model="")

    yaml_valid, _ = validate_yaml(yaml_text)
    result.yaml_valid = yaml_valid

    text_lower = yaml_text.lower()
    result.has_alias = "alias:" in text_lower
    result.has_mode = "mode:" in text_lower
    result.has_trigger = "trigger:" in text_lower or "trigger" in text_lower
    result.has_action = "action:" in text_lower or "service:" in text_lower

    result.uses_correct_entities = check_entities_used(
        yaml_text, task["required_entities"], ENTITY_IDS
    )

    result.has_required_trigger = any(
        t in text_lower for t in task["required_triggers"]
    )
    result.has_required_service = any(
        s in text_lower for s in task["required_services"]
    )

    # Score: equal weight per check
    checks = [
        result.yaml_valid, result.has_alias, result.has_mode,
        result.has_trigger, result.has_action, result.uses_correct_entities,
        result.has_required_trigger, result.has_required_service,
    ]
    result.score = round(sum(1 for c in checks if c) / len(checks), 3)
    return result


def query_model(url: str, model: str, description: str, backend: str = "ollama",
                timeout: int = 120) -> tuple[str, int, float]:
    """Query model for HA automation generation."""
    entity_list = "\n".join(
        f"  {domain}: {', '.join(ids)}"
        for domain, ids in ENTITIES.items()
    )
    prompt = (
        f"Generate a Home Assistant automation for:\n{description}\n\n"
        f"Available entities:\n{entity_list}\n\n"
        f"Output ONLY the automation YAML in a ```yaml code fence."
    )

    start = time.monotonic()
    try:
        if backend == "openai":
            resp = requests.post(
                f"{url}/v1/chat/completions",
                json={"model": model, "messages": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": prompt},
                ], "temperature": 0.1, "max_tokens": 1024},
                timeout=timeout,
            )
        else:
            resp = requests.post(
                f"{url}/api/chat",
                json={"model": model, "messages": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": prompt},
                ], "stream": False, "options": {"temperature": 0.1, "num_predict": 1024}},
                timeout=timeout,
            )

        latency = (time.monotonic() - start) * 1000
        if resp.status_code != 200:
            return f"HTTP {resp.status_code}", 0, latency

        data = resp.json()
        if backend == "openai":
            text = data["choices"][0]["message"]["content"]
            tokens = data.get("usage", {}).get("completion_tokens", 0)
        else:
            text = data.get("message", {}).get("content", "")
            tokens = data.get("eval_count", 0)
        return text, tokens, latency
    except requests.RequestException as e:
        return f"Error: {e}", 0, (time.monotonic() - start) * 1000


def run_benchmark(model: str, url: str, backend: str = "ollama") -> list[HaResult]:
    results = []
    for task in TASKS:
        print(f"  [{task['id']}] {task['name']}...", end=" ", flush=True)

        text, tokens, latency = query_model(url, model, task["description"], backend)
        if text.startswith("Error:") or text.startswith("HTTP "):
            results.append(HaResult(
                task_id=task["id"], task_name=task["name"], model=model,
                error=text, latency_ms=int(latency),
            ))
            print("ERROR")
            continue

        yaml_text = extract_yaml_block(text)
        _, parsed = validate_yaml(yaml_text)
        result = score_task(task, yaml_text, parsed)
        result.model = model
        result.latency_ms = int(latency)
        result.tok_per_sec = round((tokens / (latency / 1000)) if latency > 0 and tokens > 0 else 0, 1)
        result.response = text[:2000]

        status = "PASS" if result.score >= 0.75 else "PARTIAL" if result.score >= 0.5 else "FAIL"
        print(f"{status} (score={result.score:.2f}, yaml={result.yaml_valid}, "
              f"entities={result.uses_correct_entities}, {result.tok_per_sec:.1f}tok/s)")
        results.append(result)

    return results


def print_summary(all_results: dict[str, list[HaResult]]):
    print(f"\n{'=' * 70}")
    print("HA AUTOMATION BENCHMARK RESULTS")
    print(f"{'=' * 70}")

    for model, results in all_results.items():
        short = model.split(":")[0][-30:]
        scores = [r.score for r in results if not r.error]
        avg = sum(scores) / len(scores) if scores else 0
        yaml_ok = sum(1 for r in results if r.yaml_valid and not r.error)
        entity_ok = sum(1 for r in results if r.uses_correct_entities and not r.error)
        total = sum(1 for r in results if not r.error)
        print(f"\n  {short}:")
        print(f"    Average score: {avg:.2%}")
        print(f"    YAML valid: {yaml_ok}/{total}")
        print(f"    Correct entities: {entity_ok}/{total}")
        for r in results:
            status = "PASS" if r.score >= 0.75 else "FAIL"
            print(f"    {r.task_name:<30} {status} ({r.score:.2f})")
    print(f"{'=' * 70}")


def main():
    parser = argparse.ArgumentParser(description="Yggdrasil HA Automation Benchmark")
    parser.add_argument("--model", required=True)
    parser.add_argument("--url", default="http://localhost:11434")
    parser.add_argument("--backend", default="ollama", choices=["ollama", "openai"])
    parser.add_argument("--output", default="ha_bench_results.json")
    args = parser.parse_args()

    print(f"\nModel: {args.model}")
    print(f"URL:   {args.url} ({args.backend})")
    print(f"{'─' * 60}")

    results = run_benchmark(args.model, args.url, args.backend)
    all_results = {args.model: results}

    with open(args.output, "w") as f:
        json.dump({m: [asdict(r) for r in rs] for m, rs in all_results.items()}, f, indent=2)
    print(f"\nResults saved to {args.output}")
    print_summary(all_results)


if __name__ == "__main__":
    main()
