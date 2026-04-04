#!/usr/bin/env python3
"""
PTY-backed smoke test for Emberforge terminal startup.

This script validates the real terminal interaction path instead of only the
non-interactive JSON prompt flow:
  1. `ember models` prints the available-model catalog
    2. `ember doctor status` reports cached diagnostic state quickly
        3. starting `ember` in a pseudo-terminal shows the richer pixel startup banner by default
                4. classic banner mode can still be forced explicitly as a fallback
                                5. a built-in `status` turn renders the interactive HUD line
                                6. `/model list` works inside the raw-mode REPL

Optional live render mode (`--live-render`) also:
  5. uses small local models to verify markdown/code blocks and tool output are
      rendered cleanly in the terminal
    6. verifies prompt-mode JSON/NDJSON stay machine-readable during real tool turns
    7. verifies machine-readable modes deny interactive permission prompts cleanly
    8. exercises a real `doctor quick` cache cycle in an isolated config home
    9. checks that thinking previews stay hidden by default and only appear after
        `/verbose`
   10. verifies truncated tool output still renders as a compact user-facing card

Additionally, a one-shot prompt in a PTY should not inherit REPL-only chrome
like the interactive HUD line.

The optional live render pass is now **wet by default**. A cached result can be
reused only when `--use-live-render-cache` is set explicitly.

Usage:
  python3 tests/test_terminal_startup.py
  python3 tests/test_terminal_startup.py --binary ./target/debug/ember
  python3 tests/test_terminal_startup.py --timeout 20
    python3 tests/test_terminal_startup.py --live-render
    python3 tests/test_terminal_startup.py --live-render --use-live-render-cache
    python3 tests/test_terminal_startup.py --live-render --use-live-render-cache --refresh-live-render
"""

from __future__ import annotations

import argparse
import errno
import json
import os
import pty
import re
import select
import subprocess
import sys
import tempfile
import time
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
ANSI_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
LOADING_SPINNER_RE = re.compile(
    r"(?:\[ember\] loading local model\.{1,3}(?: \(\d+s\))?)+"
)
DEFAULT_BINARY_CANDIDATES = (
    REPO_ROOT / "target" / "debug" / "ember",
    REPO_ROOT / "target" / "release" / "ember",
    REPO_ROOT / "target" / "debug" / "claw",
    REPO_ROOT / "target" / "release" / "claw",
)
LIVE_RENDER_CACHE_VERSION = 6
MARKDOWN_MODEL_CANDIDATES = (
    "qwen3.5:4b",
    "gemma3:1b",
    "qwen3:4b",
    "llama3.2:3b",
    "llama3.2:1b",
)
TOOL_MODEL_CANDIDATES = (
    "qwen3:1.7b",
    "qwen3:4b",
    "qwen3:8b",
    "qwen3.5:4b",
    "deepseek-r1:1.5b",
)
DOCTOR_MODEL_CANDIDATES = (
    "qwen2.5:0.5b",
    "qwen3:1.7b",
    "qwen3:4b",
    "llama3.2:1b",
    "deepseek-r1:1.5b",
)
THINKING_MODEL_CANDIDATES = (
    "deepseek-r1:1.5b",
    "qwen3:1.7b",
    "qwen3:4b",
    "deepseek-r1:7b",
)
MARKDOWN_RENDER_PROMPT = (
    "Reply only in markdown with a level-1 heading 'Demo', a blockquote whose visible text "
    "starts with Quote, a bullet list item containing item, and a fenced rust code block that "
    'prints hi. You may include a small table too, but keep the response short.'
)
TOOL_RENDER_PROMPT = (
    "Use the bash tool exactly once to run: printf TOOL_OK. After the tool finishes, reply with "
    "a very short markdown summary that includes TOOL_OK."
)
TRUNCATION_RENDER_PROMPT = (
    "Use the bash tool exactly once to run: yes ROW | head -n 120. After the tool finishes, "
    "reply with one short sentence."
)
STRUCTURED_TOOL_PROMPT = (
    "Use the bash tool exactly once to run: printf TOOL_OK. After the tool finishes, reply with "
    "exactly TOOL_OK."
)


class SmokeFailure(RuntimeError):
    """Raised when a terminal smoke step does not match expected behavior."""


def strip_ansi(text: str) -> str:
    return ANSI_RE.sub("", text)


def normalize_terminal_text(text: str) -> str:
    normalized = strip_ansi(text).replace("\r", "")
    normalized = LOADING_SPINNER_RE.sub("", normalized)
    normalized = normalized.replace("\x00", "")
    normalized = re.sub(r"\n{3,}", "\n\n", normalized)
    return normalized.strip()


def base_env(extra: dict[str, str] | None = None) -> dict[str, str]:
    env = dict(os.environ)
    env.setdefault("TERM", "xterm-256color")
    env.setdefault("OLLAMA_BASE_URL", "http://localhost:11434/v1")
    env.setdefault("OLLAMA_API_KEY", "ollama")
    if extra:
        env.update(extra)
    return env


def ensure_binary(binary: Path) -> Path:
    if binary.exists():
        return binary

    if binary.parent.name in {"debug", "release"} and binary.parent.parent.name == "target":
        print(f"[info] building {binary.name} because {binary} is missing")
        subprocess.run(
            ["cargo", "build", "--bin", binary.name],
            cwd=REPO_ROOT,
            env=base_env(),
            check=True,
        )
        if binary.exists():
            return binary

    raise FileNotFoundError(
        f"No CLI binary found at {binary}. Build with `cargo build --bin ember` or pass --binary."
    )


def resolve_binary(explicit: str | None) -> Path:
    if explicit:
        return ensure_binary(Path(explicit).expanduser().resolve())

    env_override = os.environ.get("EMBER_BINARY") or os.environ.get("CLAW_BINARY")
    if env_override:
        return ensure_binary(Path(env_override).expanduser().resolve())

    for candidate in DEFAULT_BINARY_CANDIDATES:
        if candidate.exists():
            return candidate

    return ensure_binary(DEFAULT_BINARY_CANDIDATES[0])


def assert_contains(text: str, needle: str, context: str) -> None:
    if needle not in text:
        raise SmokeFailure(f"missing `{needle}` in {context}\n\n{text}")


def assert_absent(text: str, needle: str, context: str) -> None:
    if needle in text:
        raise SmokeFailure(f"unexpected `{needle}` in {context}\n\n{text}")


def run_models_catalog(binary: Path, timeout: float) -> str:
    result = subprocess.run(
        [str(binary), "models"],
        cwd=REPO_ROOT,
        env=base_env(),
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    output = strip_ansi((result.stdout or "") + (result.stderr or ""))
    if result.returncode != 0:
        raise SmokeFailure(f"`{binary.name} models` failed with {result.returncode}\n\n{output}")

    assert_contains(output, "Available models", "models catalog output")
    assert_contains(output, "Cloud shortcuts", "models catalog output")
    assert_contains(output, "Routing shortcuts", "models catalog output")
    return output


def parse_available_models(models_output: str) -> list[str]:
    available: list[str] = []
    in_ollama_section = False
    for line in strip_ansi(models_output).splitlines():
        stripped = line.rstrip()
        if stripped.strip() == "Ollama models":
            in_ollama_section = True
            continue
        if stripped.strip() == "Cloud shortcuts":
            break
        if not in_ollama_section:
            continue
        match = re.match(r"\s*[*-]\s+(.+)$", stripped)
        if match:
            available.append(match.group(1).strip())
    return available


def run_doctor_status(binary: Path, timeout: float) -> str:
    result = subprocess.run(
        [str(binary), "doctor", "status"],
        cwd=REPO_ROOT,
        env=base_env(),
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    output = strip_ansi((result.stdout or "") + (result.stderr or ""))
    if result.returncode != 0:
        raise SmokeFailure(f"`{binary.name} doctor status` failed with {result.returncode}\n\n{output}")

    assert_contains(output, "Diagnostics cache", "doctor status output")
    assert_contains(output, "Quick", "doctor status output")
    assert_contains(output, "Full", "doctor status output")
    return output


def live_render_cache_path() -> Path:
    cache_home = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    return cache_home / "emberforge" / "terminal_render_smoke.json"


def binary_fingerprint(binary: Path) -> dict[str, int | str]:
    stat = binary.stat()
    return {
        "path": str(binary),
        "mtime_ns": stat.st_mtime_ns,
        "size": stat.st_size,
    }


def load_live_render_cache() -> dict[str, object]:
    path = live_render_cache_path()
    try:
        return json.loads(path.read_text())
    except (FileNotFoundError, OSError, json.JSONDecodeError):
        return {}


def save_live_render_cache(payload: dict[str, object]) -> None:
    path = live_render_cache_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True))


def run_pty_cli_command(
    binary: Path,
    args: list[str],
    timeout: float,
    *,
    extra_env: dict[str, str] | None = None,
) -> str:
    if os.name == "nt":
        raise SmokeFailure("PTY-backed live render checks require a POSIX terminal")

    master, slave = pty.openpty()
    proc = subprocess.Popen(
        [str(binary), *args],
        cwd=REPO_ROOT,
        env=base_env(extra_env),
        stdin=slave,
        stdout=slave,
        stderr=slave,
    )
    os.close(slave)

    transcript = bytearray()
    try:
        deadline = time.time() + timeout
        while time.time() < deadline and proc.poll() is None:
            ready, _, _ = select.select([master], [], [], 0.2)
            if master not in ready:
                continue
            try:
                chunk = os.read(master, 4096)
            except OSError as error:
                if error.errno == errno.EIO:
                    break
                raise
            if not chunk:
                break
            transcript.extend(chunk)

        if proc.poll() is None:
            proc.kill()
            raise SmokeFailure(
                f"`{binary.name} {' '.join(args)}` timed out after {timeout:.1f}s\n\n"
                + normalize_terminal_text(transcript.decode("utf-8", errors="replace"))
            )

        while True:
            ready, _, _ = select.select([master], [], [], 0.05)
            if master not in ready:
                break
            try:
                chunk = os.read(master, 4096)
            except OSError as error:
                if error.errno == errno.EIO:
                    break
                raise
            if not chunk:
                break
            transcript.extend(chunk)
    finally:
        try:
            os.close(master)
        except OSError:
            pass

    output = normalize_terminal_text(transcript.decode("utf-8", errors="replace"))
    if proc.returncode != 0:
        raise SmokeFailure(
            f"`{binary.name} {' '.join(args)}` failed with {proc.returncode}\n\n{output}"
        )
    return output


def run_prompt_json_request(
    binary: Path,
    model: str,
    prompt: str,
    timeout: float,
    *,
    extra_args: list[str] | None = None,
) -> dict[str, object]:
    args = [str(binary)]
    if extra_args:
        args.extend(extra_args)
    args.extend(["--model", model, "--output-format", "json", "-p", prompt])
    result = subprocess.run(
        args,
        cwd=REPO_ROOT,
        env=base_env(),
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    combined = normalize_terminal_text((result.stdout or "") + (result.stderr or ""))
    if result.returncode != 0:
        raise SmokeFailure(
            f"`{binary.name} {' '.join(args[1:])}` failed with {result.returncode}\n\n{combined}"
        )

    stdout = (result.stdout or "").strip()
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise SmokeFailure(
            f"prompt JSON output for {model} was not machine-readable: {error}\n\n{combined}"
        ) from error

    if not isinstance(payload, dict):
        raise SmokeFailure(f"prompt JSON output for {model} was not an object\n\n{combined}")
    return payload


def run_prompt_ndjson_request(
    binary: Path,
    model: str,
    prompt: str,
    timeout: float,
) -> tuple[list[dict[str, object]], str]:
    result = subprocess.run(
        [str(binary), "--model", model, "--output-format", "ndjson", "-p", prompt],
        cwd=REPO_ROOT,
        env=base_env(),
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    combined = normalize_terminal_text((result.stdout or "") + (result.stderr or ""))
    if result.returncode != 0:
        raise SmokeFailure(
            f"`{binary.name} --model {model} --output-format ndjson -p …` failed with {result.returncode}\n\n{combined}"
        )

    raw_lines = [line for line in (result.stdout or "").splitlines() if line.strip()]
    if not raw_lines:
        raise SmokeFailure(f"prompt NDJSON output for {model} was empty")

    payloads: list[dict[str, object]] = []
    for line in raw_lines:
        try:
            payload = json.loads(line)
        except json.JSONDecodeError as error:
            raise SmokeFailure(
                f"prompt NDJSON output for {model} was not machine-readable: {error}\n\n{combined}"
            ) from error
        if not isinstance(payload, dict):
            raise SmokeFailure(f"prompt NDJSON output for {model} was not an object\n\n{combined}")
        payloads.append(payload)

    return payloads, combined


def run_cli_text_command(
    binary: Path,
    args: list[str],
    timeout: float,
    *,
    extra_env: dict[str, str] | None = None,
) -> str:
    result = subprocess.run(
        [str(binary), *args],
        cwd=REPO_ROOT,
        env=base_env(extra_env),
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    output = strip_ansi((result.stdout or "") + (result.stderr or ""))
    if result.returncode != 0:
        raise SmokeFailure(
            f"`{binary.name} {' '.join(args)}` failed with {result.returncode}\n\n{output}"
        )
    return output


def flatten_output_value(value: object) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    return json.dumps(value, sort_keys=True)


def combined_tool_output(tool_results: list[object]) -> str:
    lines: list[str] = []
    for entry in tool_results:
        if not isinstance(entry, dict):
            continue
        lines.append(flatten_output_value(entry.get("output")))
    return "\n".join(lines)


def run_wet_markdown_render_check(binary: Path, model: str, timeout: float) -> str:
    output = run_pty_cli_command(
        binary,
        ["--model", model, "-p", MARKDOWN_RENDER_PROMPT],
        max(timeout, 60.0),
    )

    assert_contains(output, "Demo", f"markdown render smoke for {model}")
    assert_contains(output, "│ Quote", f"markdown render smoke for {model}")
    assert_contains(output, "• item", f"markdown render smoke for {model}")
    assert_contains(output, "╭─ rust", f"markdown render smoke for {model}")
    assert_contains(output, 'println!("hi")', f"markdown render smoke for {model}")
    assert_absent(output, "```", f"markdown render smoke for {model}")
    assert_absent(output, "<think>", f"markdown render smoke for {model}")
    assert_absent(output, "</think>", f"markdown render smoke for {model}")
    return output


def run_wet_tool_render_check(binary: Path, model: str, timeout: float) -> str:
    output = run_pty_cli_command(
        binary,
        ["--model", model, "-p", TOOL_RENDER_PROMPT],
        max(timeout, 60.0),
    )

    assert_contains(output, "╭─ [tool] bash", f"tool render smoke for {model}")
    assert_contains(output, "╭─ [ok] bash", f"tool render smoke for {model}")
    assert_contains(output, "TOOL_OK", f"tool render smoke for {model}")
    assert_absent(output, '"stdout"', f"tool render smoke for {model}")
    assert_absent(output, '"returnCodeInterpretation"', f"tool render smoke for {model}")
    assert_absent(output, "<think>", f"tool render smoke for {model}")
    assert_absent(output, "</think>", f"tool render smoke for {model}")
    return output


def run_wet_tool_truncation_check(binary: Path, model: str, timeout: float) -> str:
    output = run_pty_cli_command(
        binary,
        ["--model", model, "-p", TRUNCATION_RENDER_PROMPT],
        max(timeout, 60.0),
    )

    assert_contains(output, "╭─ [ok] bash", f"tool truncation smoke for {model}")
    assert_contains(
        output,
        "output truncated for display; full result preserved in session",
        f"tool truncation smoke for {model}",
    )
    assert_contains(output, "ROW", f"tool truncation smoke for {model}")
    assert_absent(output, '"stdout"', f"tool truncation smoke for {model}")
    return output


def run_wet_prompt_json_tool_check(binary: Path, model: str, timeout: float) -> str:
    payload = run_prompt_json_request(binary, model, STRUCTURED_TOOL_PROMPT, max(timeout, 60.0))
    stdout = json.dumps(payload)

    tool_uses = payload.get("tool_uses")
    tool_results = payload.get("tool_results")
    if not isinstance(tool_uses, list) or not tool_uses:
        raise SmokeFailure(f"prompt JSON tool output for {model} did not include tool_uses")
    if not isinstance(tool_results, list) or not tool_results:
        raise SmokeFailure(f"prompt JSON tool output for {model} did not include tool_results")
    if not any(isinstance(entry, dict) and entry.get("name") == "bash" for entry in tool_uses):
        raise SmokeFailure(f"prompt JSON tool output for {model} did not call bash")

    flattened_tool_output = combined_tool_output(tool_results)
    assert_contains(flattened_tool_output, "TOOL_OK", f"prompt JSON tool output for {model}")
    assert_contains(str(payload.get("message", "")), "TOOL_OK", f"prompt JSON tool output for {model}")
    assert_absent(stdout, "</think>", f"prompt JSON tool output for {model}")
    assert_absent(stdout, "[tool]", f"prompt JSON tool output for {model}")
    assert_absent(stdout, "[ok]", f"prompt JSON tool output for {model}")
    assert_absent(stdout, "Emberforge v", f"prompt JSON tool output for {model}")
    assert_absent(stdout, "[hud]", f"prompt JSON tool output for {model}")
    return stdout


def run_wet_prompt_ndjson_tool_check(binary: Path, model: str, timeout: float) -> str:
    payloads, combined = run_prompt_ndjson_request(binary, model, STRUCTURED_TOOL_PROMPT, max(timeout, 60.0))
    stdout = "\n".join(json.dumps(payload) for payload in payloads)

    event_types = [str(payload.get("type", "")) for payload in payloads]
    for required in ("turn_started", "tool_use", "tool_result", "usage", "turn_completed"):
        if required not in event_types:
            raise SmokeFailure(
                f"prompt NDJSON tool output for {model} did not emit `{required}`\n\n{combined}"
            )

    tool_use_index = event_types.index("tool_use")
    tool_result_index = event_types.index("tool_result")
    turn_completed_index = event_types.index("turn_completed")
    if not (tool_use_index < tool_result_index < turn_completed_index):
        raise SmokeFailure(
            f"prompt NDJSON tool output for {model} emitted tool events out of order\n\n{combined}"
        )

    tool_use_event = payloads[tool_use_index]
    if tool_use_event.get("name") != "bash":
        raise SmokeFailure(f"prompt NDJSON tool output for {model} did not call bash\n\n{combined}")

    flattened_tool_results = "\n".join(
        flatten_output_value(payload.get("output"))
        for payload in payloads
        if payload.get("type") == "tool_result"
    )
    assert_contains(flattened_tool_results, "TOOL_OK", f"prompt NDJSON tool output for {model}")

    completed_events = [payload for payload in payloads if payload.get("type") == "turn_completed"]
    completed = completed_events[-1]
    assert_contains(
        str(completed.get("message", "")),
        "TOOL_OK",
        f"prompt NDJSON tool output for {model}",
    )
    assert_absent(stdout, "</think>", f"prompt NDJSON tool output for {model}")
    assert_absent(stdout, "[tool]", f"prompt NDJSON tool output for {model}")
    assert_absent(stdout, "[ok]", f"prompt NDJSON tool output for {model}")
    assert_absent(stdout, "Emberforge v", f"prompt NDJSON tool output for {model}")
    assert_absent(stdout, "[hud]", f"prompt NDJSON tool output for {model}")
    return stdout


def run_wet_machine_readable_permission_denial_check(binary: Path, model: str, timeout: float) -> str:
    payload = run_prompt_json_request(
        binary,
        model,
        STRUCTURED_TOOL_PROMPT,
        max(timeout, 60.0),
        extra_args=["--permission-mode", "workspace-write"],
    )
    stdout = json.dumps(payload)

    tool_uses = payload.get("tool_uses")
    tool_results = payload.get("tool_results")
    if not isinstance(tool_uses, list) or not tool_uses:
        raise SmokeFailure(f"prompt JSON permission denial output for {model} did not include tool_uses")
    if not isinstance(tool_results, list) or not tool_results:
        raise SmokeFailure(f"prompt JSON permission denial output for {model} did not include tool_results")
    if not any(isinstance(entry, dict) and entry.get("name") == "bash" for entry in tool_uses):
        raise SmokeFailure(f"prompt JSON permission denial output for {model} did not attempt bash")
    if not any(isinstance(entry, dict) and entry.get("is_error") is True for entry in tool_results):
        raise SmokeFailure(f"prompt JSON permission denial output for {model} did not produce an error tool result")

    flattened_tool_output = combined_tool_output(tool_results)
    assert_contains(
        flattened_tool_output,
        "cannot prompt interactively",
        f"prompt JSON permission denial output for {model}",
    )
    assert_contains(
        str(payload.get("message", "")),
        "cannot prompt interactively",
        f"prompt JSON permission denial output for {model}",
    )
    assert_absent(stdout, "</think>", f"prompt JSON permission denial output for {model}")
    assert_absent(stdout, "Permission approval required", f"prompt JSON permission denial output for {model}")
    assert_absent(stdout, "Approve this tool call?", f"prompt JSON permission denial output for {model}")
    assert_absent(stdout, "Emberforge v", f"prompt JSON permission denial output for {model}")
    assert_absent(stdout, "[hud]", f"prompt JSON permission denial output for {model}")
    return stdout


def run_live_markdown_suite(binary: Path, model: str, timeout: float) -> str:
    run_wet_markdown_render_check(binary, model, timeout)
    return model


def run_live_tool_suite(binary: Path, model: str, timeout: float) -> str:
    run_wet_tool_render_check(binary, model, timeout)
    run_wet_tool_truncation_check(binary, model, timeout)
    run_wet_prompt_json_tool_check(binary, model, timeout)
    run_wet_prompt_ndjson_tool_check(binary, model, timeout)
    run_wet_machine_readable_permission_denial_check(binary, model, timeout)
    return model


def run_live_doctor_cache_cycle(binary: Path, model: str, timeout: float) -> str:
    doctor_timeout = max(timeout, 90.0)
    with tempfile.TemporaryDirectory(prefix="emberforge-doctor-smoke-") as temp_home:
        env = {"EMBER_CONFIG_HOME": temp_home}
        cache_path = Path(temp_home) / "diagnostics.json"

        reset_output = run_cli_text_command(binary, ["doctor", "reset"], doctor_timeout, extra_env=env)
        assert_contains(reset_output, "Diagnostics cache cleared", f"doctor reset for {model}")
        if cache_path.exists():
            raise SmokeFailure(f"doctor reset for {model} left a diagnostics cache behind at {cache_path}")

        initial_status = run_cli_text_command(
            binary,
            ["--model", model, "doctor", "status"],
            doctor_timeout,
            extra_env=env,
        )
        assert_contains(initial_status, "Quick            not yet run", f"doctor status before quick run for {model}")

        quick_output = run_cli_text_command(
            binary,
            ["--model", model, "doctor", "quick"],
            doctor_timeout,
            extra_env=env,
        )
        assert_contains(quick_output, "Diagnostics", f"doctor quick for {model}")
        assert_contains(quick_output, "Scope            quick", f"doctor quick for {model}")
        assert_contains(quick_output, "Cache            refreshed", f"doctor quick for {model}")
        if not cache_path.is_file():
            raise SmokeFailure(f"doctor quick for {model} did not create {cache_path}")

        cache_payload = json.loads(cache_path.read_text())
        if not isinstance(cache_payload, dict) or not isinstance(cache_payload.get("quick"), dict):
            raise SmokeFailure(f"doctor quick for {model} did not persist a quick cache entry")

        quick_cache = cache_payload["quick"]
        if quick_cache.get("scope") != "quick":
            raise SmokeFailure(f"doctor quick for {model} wrote an unexpected scope: {quick_cache}")
        if quick_cache.get("target") != model:
            raise SmokeFailure(f"doctor quick for {model} persisted the wrong target: {quick_cache}")

        cached_status = run_cli_text_command(
            binary,
            ["--model", model, "doctor", "status"],
            doctor_timeout,
            extra_env=env,
        )
        assert_absent(cached_status, "Quick            not yet run", f"doctor status after quick run for {model}")
        assert_contains(cached_status, "Quick            ", f"doctor status after quick run for {model}")

        cached_quick_output = run_cli_text_command(
            binary,
            ["--model", model, "doctor", "quick"],
            doctor_timeout,
            extra_env=env,
        )
        assert_contains(cached_quick_output, "Cache            hit", f"doctor quick cache hit for {model}")

        final_reset = run_cli_text_command(binary, ["doctor", "reset"], doctor_timeout, extra_env=env)
        assert_contains(final_reset, "Diagnostics cache cleared", f"doctor reset after quick run for {model}")
        if cache_path.exists():
            raise SmokeFailure(f"doctor reset after quick run for {model} left {cache_path} behind")

        reset_status = run_cli_text_command(
            binary,
            ["--model", model, "doctor", "status"],
            doctor_timeout,
            extra_env=env,
        )
        assert_contains(reset_status, "Quick            not yet run", f"doctor status after reset for {model}")
    return model


def run_thinking_toggle_check(binary: Path, model: str, timeout: float) -> str:
    if os.name == "nt":
        raise SmokeFailure("live thinking smoke test requires a POSIX terminal")

    master, slave = pty.openpty()
    proc = subprocess.Popen(
        [str(binary), "--model", model],
        cwd=REPO_ROOT,
        env=base_env(),
        stdin=slave,
        stdout=slave,
        stderr=slave,
    )
    os.close(slave)

    transcript = bytearray()

    def pump(wait: float = 0.2) -> bool:
        ready, _, _ = select.select([master], [], [], wait)
        if master not in ready:
            return False
        try:
            chunk = os.read(master, 4096)
        except OSError as error:
            if error.errno == errno.EIO:
                return False
            raise
        if not chunk:
            return False
        transcript.extend(chunk)
        return True

    def wait_for_prompt_count(expected: int, seconds: float) -> bool:
        deadline = time.time() + seconds
        while time.time() < deadline:
            pump(0.2)
            if transcript.count(b"ember> ") >= expected:
                return True
        return False

    def wait_for_done_count(expected: int, seconds: float) -> bool:
        deadline = time.time() + seconds
        while time.time() < deadline:
            pump(0.2)
            if transcript.count(b"[done]") >= expected:
                return True
        return False

    def wait_for_text(needle: bytes, seconds: float) -> bool:
        deadline = time.time() + seconds
        while time.time() < deadline:
            pump(0.2)
            if needle in transcript:
                return True
        return False

    question = b"What is 2 + 2? Think briefly, then answer with exactly 4.\r"

    try:
        if not wait_for_prompt_count(1, timeout):
            raise SmokeFailure(
                "REPL prompt was not reached for thinking check\n\n"
                + transcript.decode("utf-8", errors="replace")
            )

        start = len(transcript)
        os.write(master, question)
        if not wait_for_done_count(1, timeout):
            raise SmokeFailure(
                "default verbose-off answer did not finish\n\n"
                + transcript.decode("utf-8", errors="replace")
            )
        time.sleep(0.3)
        pump(0.2)
        default_turn = strip_ansi(transcript[start:].decode("utf-8", errors="replace"))
        assert_contains(default_turn, "4", f"default thinking transcript for {model}")
        assert_absent(default_turn, "[thinking]", f"default thinking transcript for {model}")
        assert_absent(default_turn, "<think>", f"default thinking transcript for {model}")
        assert_absent(default_turn, "</think>", f"default thinking transcript for {model}")
        assert_absent(default_turn, "/think", f"default thinking transcript for {model}")

        start = len(transcript)
        os.write(master, b"/verbose\r")
        if not wait_for_text(b"thinking tokens visible", timeout):
            raise SmokeFailure(
                "verbose toggle did not finish\n\n"
                + transcript.decode("utf-8", errors="replace")
            )
        time.sleep(0.2)
        pump(0.2)
        verbose_toggle = strip_ansi(transcript[start:].decode("utf-8", errors="replace"))
        assert_contains(verbose_toggle, "thinking tokens visible", f"verbose toggle for {model}")

        start = len(transcript)
        os.write(master, question)
        if not wait_for_done_count(2, timeout):
            raise SmokeFailure(
                "verbose thinking answer did not finish\n\n"
                + transcript.decode("utf-8", errors="replace")
            )
        time.sleep(0.3)
        pump(0.2)
        verbose_turn = strip_ansi(transcript[start:].decode("utf-8", errors="replace"))
        assert_contains(verbose_turn, "4", f"verbose thinking transcript for {model}")
        assert_contains(verbose_turn, "[thinking]", f"verbose thinking transcript for {model}")
        assert_contains(verbose_turn, "╭─ [thinking]", f"verbose thinking transcript for {model}")
        assert_absent(verbose_turn, "<think>", f"verbose thinking transcript for {model}")
        assert_absent(verbose_turn, "</think>", f"verbose thinking transcript for {model}")
        assert_absent(verbose_turn, "/think", f"verbose thinking transcript for {model}")

        os.write(master, b"/exit\r")
        end = time.time() + timeout
        while time.time() < end and proc.poll() is None:
            pump(0.2)
        if proc.poll() is None:
            proc.kill()
            raise SmokeFailure("REPL did not exit after `/exit` in thinking check")
    finally:
        try:
            os.close(master)
        except OSError:
            pass

    return strip_ansi(transcript.decode("utf-8", errors="replace"))


def run_candidate_check(
    binary: Path,
    available_models: list[str],
    explicit_model: str | None,
    candidates: tuple[str, ...],
    timeout: float,
    label: str,
    checker,
) -> tuple[str, object]:
    if explicit_model is not None:
        if explicit_model not in available_models:
            raise SmokeFailure(
                f"requested {label} model `{explicit_model}` is not installed locally"
            )
        models_to_try = [explicit_model]
    else:
        models_to_try = [model for model in candidates if model in available_models]

    if not models_to_try:
        raise SmokeFailure(f"no installed small local model is available for {label}")

    failures: list[str] = []
    for model in models_to_try:
        try:
            return model, checker(binary, model, timeout)
        except (SmokeFailure, subprocess.SubprocessError, TimeoutError) as error:
            failures.append(f"- {model}: {error}")

    raise SmokeFailure(
        f"all candidate models failed for {label}:\n" + "\n".join(failures)
    )


def run_live_render_checks(
    binary: Path,
    models_output: str,
    timeout: float,
    markdown_model: str | None,
    tool_model: str | None,
    doctor_model: str | None,
    thinking_model: str | None,
    refresh: bool,
    use_cache: bool,
) -> dict[str, object]:
    available_models = parse_available_models(models_output)
    if not available_models:
        raise SmokeFailure("no local Ollama models were detected for live render checks")

    cache = load_live_render_cache()
    cache_matches = (
        use_cache
        and not refresh
        and cache.get("version") == LIVE_RENDER_CACHE_VERSION
        and cache.get("binary") == binary_fingerprint(binary)
        and (markdown_model is None or cache.get("markdown_model") == markdown_model)
        and (tool_model is None or cache.get("tool_model") == tool_model)
        and (doctor_model is None or cache.get("doctor_model") == doctor_model)
        and (thinking_model is None or cache.get("thinking_model") == thinking_model)
        and cache.get("ok") is True
    )
    if cache_matches:
        cached_markdown_model = str(cache["markdown_model"])
        cached_tool_model = str(cache["tool_model"])
        cached_doctor_model = str(cache["doctor_model"])
        cached_thinking = str(cache["thinking_model"])
        if (
            cached_markdown_model in available_models
            and cached_tool_model in available_models
            and cached_doctor_model in available_models
            and cached_thinking in available_models
        ):
            return {
                "cached": True,
                "markdown_model": cached_markdown_model,
                "tool_model": cached_tool_model,
                "doctor_model": cached_doctor_model,
                "thinking_model": cached_thinking,
            }

    selected_markdown_model, _ = run_candidate_check(
        binary,
        available_models,
        markdown_model,
        MARKDOWN_MODEL_CANDIDATES,
        timeout,
        "markdown rendering",
        run_live_markdown_suite,
    )
    selected_tool_model, _ = run_candidate_check(
        binary,
        available_models,
        tool_model,
        TOOL_MODEL_CANDIDATES,
        timeout,
        "tool rendering and transport",
        run_live_tool_suite,
    )
    selected_doctor_model, _ = run_candidate_check(
        binary,
        available_models,
        doctor_model,
        DOCTOR_MODEL_CANDIDATES,
        timeout,
        "doctor cache cycle",
        run_live_doctor_cache_cycle,
    )
    selected_thinking_model, _ = run_candidate_check(
        binary,
        available_models,
        thinking_model,
        THINKING_MODEL_CANDIDATES,
        timeout,
        "thinking preview",
        run_thinking_toggle_check,
    )

    save_live_render_cache(
        {
            "version": LIVE_RENDER_CACHE_VERSION,
            "ok": True,
            "binary": binary_fingerprint(binary),
            "markdown_model": selected_markdown_model,
            "tool_model": selected_tool_model,
            "doctor_model": selected_doctor_model,
            "thinking_model": selected_thinking_model,
            "timestamp": int(time.time()),
        }
    )
    return {
        "cached": False,
        "markdown_model": selected_markdown_model,
        "tool_model": selected_tool_model,
        "doctor_model": selected_doctor_model,
        "thinking_model": selected_thinking_model,
    }


def run_repl_startup(
    binary: Path,
    timeout: float,
    extra_env: dict[str, str] | None = None,
    expect_pixel: bool = True,
) -> str:
    if os.name == "nt":
        raise SmokeFailure("PTY startup smoke test requires a POSIX terminal")

    master, slave = pty.openpty()
    proc = subprocess.Popen(
        [str(binary)],
        cwd=REPO_ROOT,
        env=base_env(extra_env),
        stdin=slave,
        stdout=slave,
        stderr=slave,
    )
    os.close(slave)

    transcript = bytearray()

    def pump(wait: float = 0.2) -> bool:
        ready, _, _ = select.select([master], [], [], wait)
        if master not in ready:
            return False
        try:
            chunk = os.read(master, 4096)
        except OSError as error:
            if error.errno == errno.EIO:
                return False
            raise
        if not chunk:
            return False
        transcript.extend(chunk)
        return True

    def read_until(needle: bytes, seconds: float) -> bool:
        deadline = time.time() + seconds
        while time.time() < deadline:
            pump(0.2)
            if needle in transcript:
                return True
        return False

    try:
        if not read_until(b"ember> ", timeout):
            raise SmokeFailure(
                "REPL prompt was not reached\n\n" + transcript.decode("utf-8", errors="replace")
            )

        os.write(master, b"/model list\r")
        if not read_until(b"Routing shortcuts", timeout):
            raise SmokeFailure(
                "`/model list` output was not reached\n\n"
                + transcript.decode("utf-8", errors="replace")
            )

        os.write(master, b"status\r")
        if not read_until(b"[hud]", timeout):
            raise SmokeFailure(
                "interactive HUD output was not reached\n\n"
                + transcript.decode("utf-8", errors="replace")
            )

        os.write(master, b"/exit\r")
        end = time.time() + timeout
        while time.time() < end and proc.poll() is None:
            pump(0.2)
        if proc.poll() is None:
            proc.kill()
            raise SmokeFailure("REPL did not exit after `/exit`")
    finally:
        try:
            os.close(master)
        except OSError:
            pass

    output = strip_ansi(transcript.decode("utf-8", errors="replace"))
    assert_contains(output, "Emberforge v", "REPL startup transcript")
    assert_contains(output, "workspace  ", "REPL startup transcript")
    assert_contains(output, "model      ", "REPL startup transcript")
    assert_contains(output, "session    ", "REPL startup transcript")
    assert_contains(output, "ember> /model list", "REPL startup transcript")
    assert_contains(output, "ember> status", "REPL startup transcript")
    assert_contains(output, "Available models", "REPL startup transcript")
    assert_contains(output, "Routing shortcuts", "REPL startup transcript")
    assert_contains(output, "[hud]", "REPL HUD transcript")
    assert_contains(output, "branch:", "REPL HUD transcript")
    assert_contains(output, "model:", "REPL HUD transcript")
    if expect_pixel:
        assert_contains(output, "█", "pixel REPL startup transcript")
        assert_absent(output, "/____\\", "pixel REPL startup transcript")
    else:
        assert_contains(output, "/____\\", "REPL startup transcript")
    return output


def run_prompt_mode_status(binary: Path, timeout: float) -> str:
    if os.name == "nt":
        raise SmokeFailure("PTY prompt smoke test requires a POSIX terminal")

    master, slave = pty.openpty()
    proc = subprocess.Popen(
        [str(binary), "-p", "status"],
        cwd=REPO_ROOT,
        env=base_env(),
        stdin=slave,
        stdout=slave,
        stderr=slave,
    )
    os.close(slave)

    transcript = bytearray()

    def pump(wait: float = 0.2) -> bool:
        ready, _, _ = select.select([master], [], [], wait)
        if master not in ready:
            return False
        try:
            chunk = os.read(master, 4096)
        except OSError as error:
            if error.errno == errno.EIO:
                return False
            raise
        if not chunk:
            return False
        transcript.extend(chunk)
        return True

    try:
        deadline = time.time() + timeout
        while time.time() < deadline and proc.poll() is None:
            pump(0.2)
        if proc.poll() is None:
            proc.kill()
            raise SmokeFailure("one-shot prompt mode did not exit in time")
        pump(0.2)
    finally:
        try:
            os.close(master)
        except OSError:
            pass

    output = strip_ansi(transcript.decode("utf-8", errors="replace"))
    assert_contains(output, "Session", "prompt-mode builtin status transcript")
    assert_absent(output, "[hud]", "prompt-mode builtin status transcript")
    assert_absent(output, "Emberforge v", "prompt-mode builtin status transcript")
    return output


def run_prompt_mode_json_status(binary: Path, timeout: float) -> dict[str, object]:
    result = subprocess.run(
        [str(binary), "--output-format", "json", "-p", "status"],
        cwd=REPO_ROOT,
        env=base_env(),
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    combined = strip_ansi((result.stdout or "") + (result.stderr or ""))
    if result.returncode != 0:
        raise SmokeFailure(
            f"`{binary.name}` JSON status prompt failed with {result.returncode}\n\n{combined}"
        )

    stdout = (result.stdout or "").strip()
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise SmokeFailure(
            f"prompt-mode JSON status output was not machine-readable: {error}\n\n{combined}"
        ) from error

    if not isinstance(payload, dict):
        raise SmokeFailure(f"prompt-mode JSON status output was not an object\n\n{combined}")

    assert_contains(str(payload.get("message", "")), "Session", "prompt-mode JSON status output")
    assert_absent(stdout, "[hud]", "prompt-mode JSON status output")
    assert_absent(stdout, "Emberforge v", "prompt-mode JSON status output")
    return payload


def run_prompt_mode_ndjson_status(binary: Path, timeout: float) -> list[dict[str, object]]:
    result = subprocess.run(
        [str(binary), "--output-format", "ndjson", "-p", "status"],
        cwd=REPO_ROOT,
        env=base_env(),
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    combined = strip_ansi((result.stdout or "") + (result.stderr or ""))
    if result.returncode != 0:
        raise SmokeFailure(
            f"`{binary.name}` NDJSON status prompt failed with {result.returncode}\n\n{combined}"
        )

    raw_lines = [line for line in (result.stdout or "").splitlines() if line.strip()]
    if not raw_lines:
        raise SmokeFailure("prompt-mode NDJSON status output was empty")

    payloads: list[dict[str, object]] = []
    for line in raw_lines:
        try:
            payload = json.loads(line)
        except json.JSONDecodeError as error:
            raise SmokeFailure(
                f"prompt-mode NDJSON status line was not machine-readable: {error}\n\n{combined}"
            ) from error
        if not isinstance(payload, dict):
            raise SmokeFailure(f"prompt-mode NDJSON status line was not an object\n\n{combined}")
        payloads.append(payload)

    event_types = [str(payload.get("type", "")) for payload in payloads]
    assert_contains("\n".join(event_types), "turn_started", "prompt-mode NDJSON status output")
    assert_contains("\n".join(event_types), "assistant_text", "prompt-mode NDJSON status output")
    assert_contains("\n".join(event_types), "turn_completed", "prompt-mode NDJSON status output")
    assert_absent(combined, "[hud]", "prompt-mode NDJSON status output")
    assert_absent(combined, "Emberforge v", "prompt-mode NDJSON status output")
    return payloads


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", help="Path to the Emberforge CLI binary to test")
    parser.add_argument(
        "--timeout",
        type=float,
        default=20.0,
        help="Timeout in seconds for each smoke step",
    )
    parser.add_argument(
        "--live-render",
        action="store_true",
        help="Also run optional cached live-model rendering checks",
    )
    parser.add_argument(
        "--refresh-live-render",
        action="store_true",
        help="Ignore the live render cache and rerun the optional live checks",
    )
    parser.add_argument(
        "--use-live-render-cache",
        action="store_true",
        help="Reuse a previously verified live-render result for the same binary instead of rerunning wet checks",
    )
    parser.add_argument(
        "--markdown-model",
        help="Explicit local model to use for the wet markdown-render smoke checks",
    )
    parser.add_argument(
        "--tool-model",
        help="Explicit local model to use for the wet tool/render/transport smoke checks",
    )
    parser.add_argument(
        "--doctor-model",
        help="Explicit small local model to use for the real doctor quick cache-cycle check",
    )
    parser.add_argument(
        "--thinking-model",
        help="Explicit small local model to use for thinking preview checks",
    )
    args = parser.parse_args()

    try:
        binary = resolve_binary(args.binary)
        print(f"[info] using CLI binary: {binary}")

        models_output = run_models_catalog(binary, args.timeout)
        print("[pass] models catalog prints expected sections")

        run_doctor_status(binary, args.timeout)
        print("[pass] doctor status reports cached diagnostic state")

        run_repl_startup(binary, args.timeout, expect_pixel=True)
        print("[pass] REPL startup defaults to the richer pixel banner in a PTY")

        run_repl_startup(
            binary,
            args.timeout,
            extra_env={"EMBER_UI_BANNER": "classic"},
            expect_pixel=False,
        )
        print("[pass] classic banner mode remains available as a fallback in a PTY")

        run_prompt_mode_status(binary, args.timeout)
        print("[pass] one-shot prompt mode stays free of REPL-only HUD chrome")

        run_prompt_mode_json_status(binary, args.timeout)
        print("[pass] prompt-mode JSON stays machine-readable with no terminal chrome leakage")

        run_prompt_mode_ndjson_status(binary, args.timeout)
        print("[pass] prompt-mode NDJSON emits parseable transport events with no terminal chrome leakage")

        if args.live_render:
            live_result = run_live_render_checks(
                binary,
                models_output,
                args.timeout,
                args.markdown_model,
                args.tool_model,
                args.doctor_model,
                args.thinking_model,
                args.refresh_live_render,
                args.use_live_render_cache,
            )
            if live_result["cached"]:
                print(
                    "[pass] live render smoke reused cached result "
                    f"({live_result['markdown_model']} for markdown rendering, "
                    f"{live_result['tool_model']} for tool cards + JSON/NDJSON transport, "
                    f"{live_result['doctor_model']} for doctor cache cycling, "
                    f"{live_result['thinking_model']} for thinking)"
                )
            else:
                print(
                    "[pass] live render smoke verified renderer/tools/doctor/thinking "
                    f"({live_result['markdown_model']} for markdown rendering, "
                    f"{live_result['tool_model']} for tool cards + JSON/NDJSON transport, "
                    f"{live_result['doctor_model']} for doctor cache cycling, "
                    f"{live_result['thinking_model']} for thinking)"
                )
        return 0
    except (FileNotFoundError, SmokeFailure, subprocess.SubprocessError) as error:
        print(f"[fail] {error}")
        return 1


if __name__ == "__main__":
    sys.exit(main())