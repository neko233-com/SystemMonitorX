#!/usr/bin/env python3
"""Verify owned Rust UI sources and eight-locale catalogs without changing source.

Run: python scripts/verify-ui-localization.py --check
     python scripts/verify-ui-localization.py --self-test

This is a lexical audit with documented, narrow presentation-flow rules, not a
Rust semantic analyzer. Unproved flows remain review findings. Reports and the
regenerated source list are written only to .cache/i18n-plan.
"""
from __future__ import annotations

import argparse
import bisect
from collections import Counter
from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import sys

LOCALES = ("zh-CN", "en-US", "ja-JP", "ko-KR", "de-DE", "fr-FR", "es-ES", "pt-BR")
HAN = re.compile(r"[\u3400-\u4dbf\u4e00-\u9fff]")
WRAPPERS = {
    "tr": 0, "trs": 0, "tf": 0, "msg": 0, "mf": 0, "mlf": 0,
    "trs_in": 1, "tf_in": 1, "text_source": 0, "text_source_in": 1,
    "format_source": 0, "format_source_in": 1,
}
SHARED_HELPERS: set[str] = set()
FORMATS = {"tf", "tf_in", "mf", "mlf", "format_source", "format_source_in"}
GUI = {
    "child", "label", "title", "description", "ok_text", "cancel_text",
    "placeholder", "set_placeholder", "tooltip", "detail_row", "chart",
}
OWNERS = {
    "hardware.rs": "windows_operations", "management.rs": "windows_operations",
    "tables.rs": "gpu_collector", "directx.rs": "language_catalog",
    "process_details.rs": "language_catalog",
}
# Technical names and units remain stable across locales. Unlike a blanket
# ASCII exclusion, this allow-list does not hide new authored English labels.
NEUTRAL = {
    "", "—", "CPU", "GPU", "PID", "SMT", "LUID", "WDDM", "DXGI", "PCI",
    "VBIOS", "DirectX 12", "Mesh Shader", "Sampler Feedback", "Enhanced Barriers",
    "CPU %", "GPU %", "DirectX 12 / DXGI", "CPU / NUMA", "NVML", "NVMe",
}
BACKEND_INPUTS = {
    "src/monitor.rs": [
        "系统空闲", "状态不可用",
        "此设备未向 Windows 提供可读取的温度传感器。GPU 温度取决于显卡驱动支持。",
        "系统温度来自 Windows 热区传感器，不一定对应 CPU 核心温度。",
        "未检测到可用的图形适配器。", "部分卷未提供 I/O 计数；相关速率显示为不可用。",
    ],
    "src/platform.rs": [
        "正在启动", "正在停止", "正在继续", "正在暂停", "未知状态", "无法读取配置",
        "引导启动", "系统启动", "手动", "未知启动类型", "HKLM · Run (64 位)",
    ],
    "src/sessions.rs": [
        "活动", "已断开", "空闲", "监听", "重置中", "已关闭", "初始化中",
        "连接中", "本地控制台", "协议 {value}",
    ],
    "src/network_ports.rs": ["未获得进程身份"],
    "src/process_groups.rs": [
        "成员按已核验父子关系分组；GPU 显示成员最忙占用的最大值，不代表整组引擎总占用。",
        "应用分组依据当前桌面的可见顶层窗口；其他桌面、用户会话或托管窗口可能不可见，未确认的进程保持独立。",
    ],
    "src/gpu.rs": [
        "GPU 性能计数器不可用；需要 WDDM 2.0+ 驱动", "GPU 正在建立采样基线",
        "此 GPU 没有有效引擎样本；检查 WDDM 驱动",
        "引擎正常；部分显存计数器不可用", "WDDM / DXGI 实时采样",
        "NVIDIA 驱动未提供可用的 NVML；温度、功耗、风扇和频率不可用",
        "通用 WDDM 提供利用率和显存；此设备未接入厂商温度、功耗及风扇接口",
        "无法取得此 LUID 的 PCI 位置，未绑定 NVIDIA 传感器",
        "NVML 与 Windows PCI 标识无法唯一对应，未绑定此设备传感器",
        "NVIDIA NVML · PCI 唯一匹配",
        "NVML 驱动未提供部分传感器读数；对应项目显示为不可用",
    ],
}

@dataclass
class Token:
    kind: str
    text: str
    start: int
    end: int
    index: int = 0
    parents: tuple[int, ...] = ()
    close: int | None = None

@dataclass
class Call:
    name: str
    open: int
    close: int
    ranges: list[tuple[int, int]]
    start: int
    end: int

    @property
    def short(self) -> str:
        return self.name.rsplit("::", 1)[-1]

@dataclass
class Function:
    name: str
    start: int
    body: int
    end: int

@dataclass
class Model:
    tokens: list[Token]
    calls: list[Call]
    functions: list[Function]

def decode_rust(value: str) -> str:
    def replace(match: re.Match[str]) -> str:
        if match.group(1):
            return chr(int(match.group(1).replace("_", ""), 16))
        if match.group(2):
            return chr(int(match.group(2), 16))
        return {'"': '"', "'": "'", "\\": "\\", "n": "\n",
                "r": "\r", "t": "\t", "0": "\0"}[match.group(3)]
    # Rust's escaped physical newline also removes following whitespace.
    value = re.sub(r"\\\r?\n\s*", "", value)
    return re.sub(r"""\\u\{([0-9a-fA-F_]+)\}|\\x([0-9a-fA-F]{2})|\\(["'\\nrt0])""",
                  replace, value)

def tokenize(source: str) -> list[Token]:
    tokens: list[Token] = []
    i = 0
    while i < len(source):
        if source[i].isspace():
            i += 1
            continue
        if source.startswith("//", i):
            end = source.find("\n", i + 2)
            i = len(source) if end < 0 else end + 1
            continue
        if source.startswith("/*", i):
            depth, i = 1, i + 2
            while i < len(source) and depth:
                if source.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif source.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            if depth:
                raise ValueError("unterminated block comment")
            continue
        raw = re.match(r'(?:br|r)(#*)"', source[i:])
        if raw:
            start, body = i, i + raw.end()
            closing = '"' + raw.group(1)
            end = source.find(closing, body)
            if end < 0:
                raise ValueError("unterminated raw string")
            i = end + len(closing)
            tokens.append(Token("string", source[body:end], start, i))
            continue
        if source[i] == '"' or source.startswith('b"', i):
            start = i
            i += 2 if source.startswith('b"', i) else 1
            body = i
            while i < len(source):
                if source[i] == "\\":
                    i += 2
                elif source[i] == '"':
                    break
                else:
                    i += 1
            if i >= len(source):
                raise ValueError("unterminated string")
            value, i = decode_rust(source[body:i]), i + 1
            tokens.append(Token("string", value, start, i))
            continue
        if source[i] == "'":
            char = re.match(r"'(?:\\u\{[0-9a-fA-F_]+\}|\\.|[^'\\])'", source[i:])
            if char:
                tokens.append(Token("char", char.group(), i, i + char.end()))
                i += char.end()
                continue
        ident = re.match(r"[A-Za-z_][A-Za-z_0-9]*", source[i:])
        if ident:
            tokens.append(Token("ident", ident.group(), i, i + ident.end()))
            i += ident.end()
            continue
        two = source[i:i + 2]
        punct = two if two in ("::", "=>", "->", "&&", "||", "==", "!=", ">=", "<=") else source[i]
        tokens.append(Token("punct", punct, i, i + len(punct)))
        i += len(punct)
    stack: list[int] = []
    for index, token in enumerate(tokens):
        token.index, token.parents = index, tuple(stack)
        if token.text in ("(", "[", "{"):
            stack.append(index)
        elif token.text in (")", "]", "}"):
            if not stack:
                raise ValueError("unbalanced delimiter")
            opening = stack.pop()
            if {"(": ")", "[": "]", "{": "}"}[tokens[opening].text] != token.text:
                raise ValueError("mismatched delimiter")
            tokens[opening].close = index
    if stack:
        raise ValueError("unclosed delimiter")
    return tokens

def model(source: str) -> Model:
    tokens, calls, functions = tokenize(source), [], []
    for index, token in enumerate(tokens):
        if token.text == "(" and token.close is not None:
            previous = index - 1
            if previous >= 0 and tokens[previous].text == "!":
                previous -= 1
            if previous >= 0 and tokens[previous].kind == "ident":
                start = previous
                while start >= 2 and tokens[start - 1].text == "::" and tokens[start - 2].kind == "ident":
                    start -= 2
                name = "".join(t.text for t in tokens[start:previous + 1])
                ranges, begin, cursor = [], index + 1, index + 1
                while cursor < token.close:
                    if tokens[cursor].text in ("(", "[", "{") and tokens[cursor].close is not None:
                        cursor = tokens[cursor].close + 1
                        continue
                    if tokens[cursor].text == ",":
                        ranges.append((begin, cursor))
                        begin = cursor + 1
                    cursor += 1
                if begin < token.close:
                    ranges.append((begin, token.close))
                calls.append(Call(name, index, token.close, ranges,
                                  tokens[start].start, tokens[token.close].end))
        if token.text == "fn" and index + 1 < len(tokens) and tokens[index + 1].kind == "ident":
            cursor = index + 2
            while cursor < len(tokens):
                current = tokens[cursor]
                if current.text == ";":
                    break
                if current.text in ("(", "["):
                    cursor = current.close + 1
                    continue
                if current.text == "{":
                    functions.append(Function(tokens[index + 1].text, token.start,
                                              current.start, tokens[current.close].end))
                    break
                cursor += 1
    return Model(tokens, calls, functions)

def inside(index: int, span: tuple[int, int] | None) -> bool:
    return span is not None and span[0] <= index < span[1]

def context(m: Model, token: Token) -> tuple[Function | None, list[Call]]:
    funcs = [fn for fn in m.functions if fn.start <= token.start and token.end <= fn.end]
    fn = min(funcs, key=lambda f: f.end - f.start) if funcs else None
    parents = sorted((call for call in m.calls if call.open < token.index < call.close),
                     key=lambda c: c.close - c.open)
    return fn, parents

def is_wrapper(call: Call) -> bool:
    # anyhow::Error::msg creates an error; it is not our typed UI msg helper.
    return call.short in WRAPPERS and not (call.short == "msg" and "Error::" in call.name)

def arg(call: Call, index: int) -> tuple[int, int] | None:
    return call.ranges[index] if len(call.ranges) > index else None

def helper_translates(source: str, m: Model, name: str, parameter: str) -> bool:
    fn = next((f for f in m.functions if f.name == name), None)
    if fn is None:
        return name in SHARED_HELPERS
    body = source[fn.body:fn.end]
    direct = re.search(r"\b(?:tr|trs|msg)\(\s*&?" + re.escape(parameter) + r"\s*\)", body)
    progress = name == "run_operation" and "UiMessage::Progress(label.to_owned())" in body
    return bool(direct or progress)

def deferred_reason(file: str, source: str, m: Model, token: Token,
                    fn: Function | None, parents: list[Call]) -> tuple[str, bool] | None:
    body = source[fn.body:fn.end] if fn else source
    if fn and fn.name == "source_title" and "tr(self.source_title())" in source:
        return ("Page::title translates source_title", True)
    if fn and fn.name == "search_source" and "tr(self.page.search_source())" in source:
        return ("InputState placeholder translates Page::search_source", True)
    if any(c.short == "matches" for c in parents):
        return ("matches! comparison token", False)
    if any(c.short in ("strip_prefix", "starts_with") for c in parents):
        return ("Raw backend protocol/architecture comparison prefix", False)
    if fn and fn.name == "architecture_text" and token.index + 1 < len(m.tokens) and m.tokens[token.index + 1].text == "=>":
        return ("Raw architecture comparison token; branch translates its output", False)
    if fn and fn.name == "session_protocol" and token.index + 1 < len(m.tokens) and m.tokens[token.index + 1].text == ")":
        return ("Raw session protocol comparison token", False)
    if any(c.name == "Column::new" and inside(token.index, arg(c, 1)) for c in parents) and re.search(r"column\.name\s*=\s*tr\(", source):
        return ("Column delegate translates stored header source", True)
    if any(c.name == "TextTable::new" for c in parents):
        return ("TextTable delegate translates stored header source", True)
    if file == "ports.rs" and fn and fn.name == "new" and re.search(r"column\.name\s*=\s*tr\(", source):
        return ("Port delegate translates stored header source", True)
    if file == "hardware.rs" and any(c.short == "boolean" and (inside(token.index, arg(c, 2)) or inside(token.index, arg(c, 3))) for c in parents):
        if re.search(r"fn boolean[\s\S]*?trs_in\(language,\s*if value", source):
            return ("Captured-language boolean helper translates source branches", True)
    for helper, ordinal, parameter in (("run_operation", 0, "label"), ("confirm_management", 0, "title"), ("chart", 0, "title")):
        if any(c.short == helper and inside(token.index, arg(c, ordinal)) for c in parents) and helper_translates(source, m, helper, parameter):
            return (helper + " translates its owned " + parameter, True)
    if fn and fn.name == "service_state_text" and "trs(source)" in body:
        return ("Typed ServiceState match source translated by trs(source)", True)
    if file == "mod.rs" and fn and fn.name == "refresh_metadata" and token.text in ("已启用", "已禁用", "不可用"):
        if ".localized_columns(&[1, 3])" in source:
            return ("Startup status is preserved raw and translated by localized_columns[1]", True)
    for opening in token.parents:
        if m.tokens[opening].text == "[":
            end = m.tokens[opening].close
            after = source[m.tokens[end].end:min(fn.end if fn else len(source), m.tokens[end].end + 1000)]
            if re.search(r"\.map\s*\(\s*\|[^|]*\b(?:label|name)\b[^|]*\|[\s\S]*?(?:tr|trs|msg)\(\s*(?:label|name)\s*\)", after):
                return ("Inline source-label array has a translated map sink", True)
    if fn and fn.name == "context_menu" and re.search(r"\.child\(tr\(label\)\)", body):
        binding = "choices" if file == "ports.rs" else "items"
        start = body.find("let " + binding + " =")
        finish = body.find(";", start)
        if start >= 0 and start <= token.start - fn.body < finish:
            return ("Context menu source-label array rendered through tr(label)", True)
    if fn and fn.name in ("handle_process_action", "management_toolbar", "load_process_controls"):
        owned_labels = {"结束应用", "结束进程树", "结束进程", "禁用启动应用", "启用启动应用", "启用效率模式", "关闭效率模式"}
        if token.text in owned_labels and re.search(r"confirm_management\(\s*label\s*,", body) and helper_translates(source, m, "confirm_management", "title"):
            return ("Owned local label binding is translated by confirm_management", True)
    if file == "mod.rs" and fn and fn.name == "metric_strip" and ".child(tr(name))" in body:
        return ("Metric names rendered through tr(name)", True)
    if file == "mod.rs" and fn and fn.name == "performance" and re.search(r"detail_row\(tr\(k\)", body):
        return ("Performance detail source labels translated by tr(k)", True)
    if file == "directx.rs" and fn and fn.name == "directx_details" and token.text in ("功能级别 / Shader Model", "光线追踪", "GPU 节点 / LUID") and "tr(label)" in body:
        return ("DirectX detail-row source labels translated at sink", True)
    if file == "tables.rs" and fn and fn.name == "selected_application_unavailable_reason":
        return ("Owned group-selection reason is rendered through diagnostic(reason) by caller", True)
    return None

def rust_slots(source: str) -> Counter[str]:
    value = source.replace("{{", "").replace("}}", "")
    slots, anonymous = [], 0
    for match in re.finditer(r"\{([^{}]*)\}", value):
        field = match.group(1).split(":", 1)[0]
        if not field:
            name, anonymous = "arg" + str(anonymous), anonymous + 1
        elif field.isdigit():
            name = "arg" + field
        elif re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*", field):
            name = field
        else:
            raise ValueError("unsupported Rust placeholder " + match.group())
        slots.append(name)
    return Counter(slots)

def runtime_slots(value: str) -> Counter[str]:
    return Counter(re.findall(r"%\{([A-Za-z_][A-Za-z_0-9]*)\}", value))

def template_arg_names(m: Model, span: tuple[int, int] | None) -> list[str] | None:
    if span is None:
        return None
    selected = m.tokens[span[0]:span[1]]
    # Accept only literal &[("slot", expression), ...], not an indirect variable.
    opening = next((t for t in selected if t.text == "["), None)
    if opening is None or opening.close is None:
        return None
    names, index = [], opening.index + 1
    while index < opening.close:
        token = m.tokens[index]
        if token.text == ",":
            index += 1
            continue
        if token.text != "(" or token.close is None:
            return None
        if index + 2 >= token.close or m.tokens[index + 1].kind != "string" or m.tokens[index + 2].text != ",":
            return None
        names.append(m.tokens[index + 1].text)
        index = token.close + 1
    return names

def audit_text(file: str, source: str, aliases: dict, catalog: dict) -> dict:
    test_start = source.find("#[cfg(test)]")
    if test_start >= 0:
        source = source[:test_start]
    m, findings, used, exclusions, template_checks = model(source), [], {}, [], []
    newlines = [-1] + [match.start() for match in re.finditer("\n", source)]
    def line(token: Token) -> int:
        return bisect.bisect_left(newlines, token.start)
    def add(token: Token, category: str, message: str, **extra: object) -> None:
        fn, _ = context(m, token)
        findings.append(dict(file="src/ui/" + file, line=line(token),
                             function=fn.name if fn else None, source=token.text,
                             owner=OWNERS.get(file, "root"), category=category,
                             message=message, **extra))
    def register(token: Token, reason: str) -> None:
        if token.text in NEUTRAL and token.text not in aliases:
            exclusions.append(dict(file="src/ui/" + file, line=line(token),
                                   source=token.text, reason="Explicit technical neutral source"))
            return
        used[token.text] = dict(source=token.text, file="src/ui/" + file,
                               line=line(token), reason=reason)
        key = aliases.get(token.text)
        if not key:
            add(token, "catalog_missing", "Used source has no stable catalog alias")
            return
        entry = catalog.get(key)
        if not isinstance(entry, dict) or any(not isinstance(entry.get(locale), str) or not entry[locale].strip() for locale in LOCALES):
            add(token, "catalog_incomplete", "Missing or empty locale translation", key=key)

    for token in (t for t in m.tokens if t.kind == "string"):
        fn, parents = context(m, token)
        translated = next((c for c in parents if is_wrapper(c) and inside(token.index, arg(c, WRAPPERS[c.short]))), None)
        if translated:
            # Nested literal calls such as format!("...", dynamic) are not a source.
            nearer = parents[:parents.index(translated)]
            if any(c.short == "format" for c in nearer):
                if HAN.search(token.text):
                    add(token, "dynamic_source_review", "Formatted value was supplied as a source; translate the static template instead")
                continue
            register(token, "source argument of " + translated.short)
            continue
        if not HAN.search(token.text):
            continue
        deferred = deferred_reason(file, source, m, token, fn, parents)
        if deferred:
            exclusions.append(dict(file="src/ui/" + file, line=line(token),
                                   source=token.text, reason=deferred[0]))
            if deferred[1]:
                register(token, deferred[0])
            continue
        register(token, "unwrapped owned-source candidate")
        if any(c.short in FORMATS for c in parents):
            add(token, "untranslated_embedded_argument", "Owned Chinese phrase in template data needs its own tr/trs/msg wrapper")
        elif any(c.short in GUI for c in parents):
            add(token, "untranslated_gui_literal", "Chinese text reaches a GUI sink without a translation wrapper")
        elif any(c.short in ("context", "ensure", "bail", "anyhow") for c in parents):
            add(token, "untranslated_ui_diagnostic", "UI-authored static diagnostic is displayed as raw error text")
        elif token.text.startswith("\ufeff") and "," in token.text:
            add(token, "export_schema_review", "CSV schema is outside GUI scope; decide on stable field names")
        else:
            add(token, "unreviewed_literal", "Presentation flow is not proven; inspect this literal and its callers")

    for call in (c for c in m.calls if is_wrapper(c)):
        ordinal = WRAPPERS[call.short]
        span = arg(call, ordinal)
        if span is None:
            continue
        source_tokens = []
        for candidate in m.tokens[span[0]:span[1]]:
            if candidate.kind != "string":
                continue
            _, candidate_parents = context(m, candidate)
            owner = next((c for c in candidate_parents if is_wrapper(c) and inside(candidate.index, arg(c, WRAPPERS[c.short]))), None)
            if owner is call:
                source_tokens.append(candidate)
        if not source_tokens:
            # Wrappers with variable sources are deliberately reported separately.
            template_checks.append(dict(file="src/ui/" + file,
                                        line=line(m.tokens[call.open]),
                                        helper=call.short, status="deferred_variable_source"))
            continue
        provided = template_arg_names(m, arg(call, ordinal + 1)) if call.short in FORMATS else []
        for token in source_tokens:
            if token.text in NEUTRAL and token.text not in aliases:
                continue
            key = aliases.get(token.text)
            if not key or key not in catalog:
                continue
            expected = set(runtime_slots(catalog[key].get("en-US", "")))
            if provided is None:
                template_checks.append(dict(file="src/ui/" + file, line=line(token),
                                            helper=call.short, source=token.text,
                                            status="deferred_variable_arguments"))
                continue
            observed = set(provided)
            if expected != observed or len(provided) != len(observed):
                add(token, "template_arguments", "Template call supplies missing, extra or duplicate argument keys",
                    key=key, expected=sorted(expected), provided=provided)
            template_checks.append(dict(file="src/ui/" + file, line=line(token),
                                        helper=call.short, source=token.text,
                                        status="verified_literal_arguments"))
    # Only these bindings are known authored UI labels; never infer that arbitrary
    # process.name, path, signer, user or device names should be translated.
    for call in (c for c in m.calls if c.short in FORMATS):
        fn, _ = context(m, m.tokens[call.open])
        roles = []
        if file == "mod.rs" and fn and fn.name == "process_inspector":
            roles = ["name"]
        elif file == "management.rs" and fn and fn.name in ("management_toolbar", "users_view"):
            roles = ["label"]
        elif file == "developer_actions.rs" and fn and fn.name in ("handle_process_action", "copy_developer_text"):
            roles = ["label"]
        elif file == "columns.rs" and fn and fn.name == "columns_dialog":
            roles = ["name"]
        span = arg(call, WRAPPERS[call.short] + 1)
        if not span:
            continue
        fragment = source[m.tokens[span[0]].start:m.tokens[span[1] - 1].end]
        body = source[fn.body:fn.end] if fn else ""
        for role in roles:
            raw = re.search(r'format!\(\s*"\{' + role + r'(?::[^}]*)?\}"\s*\)', fragment)
            binding = re.search(r"let\s+(?:mut\s+)?" + role + r"\s*=\s*(?:tr|trs)\(", body)
            if raw and not binding:
                add(m.tokens[call.open], "untranslated_owned_argument_binding",
                    "Known owned UI label is captured raw inside a template", binding=role)
    return dict(findings=findings, used=list(used.values()), exclusions=exclusions,
                template_checks=template_checks)

def validate_catalogs(catalogs: dict[str, dict], aliases: dict) -> list[dict]:
    findings = []
    for filename, catalog in catalogs.items():
        for key, entry in catalog.items():
            if key == "_version":
                continue
            if not isinstance(entry, dict):
                findings.append(dict(file=filename, key=key, category="invalid_catalog_entry"))
                continue
            baseline = None
            for locale in LOCALES:
                value = entry.get(locale)
                if not isinstance(value, str) or not value.strip():
                    findings.append(dict(file=filename, key=key, locale=locale, category="catalog_incomplete"))
                    continue
                slots = runtime_slots(value)
                if baseline is None:
                    baseline = slots
                elif slots != baseline:
                    findings.append(dict(file=filename, key=key, locale=locale,
                                         category="catalog_placeholder_mismatch"))
    app = catalogs["locales/app.json"]
    for source, key in aliases.items():
        entry = app.get(key)
        if entry is None:
            findings.append(dict(file="locales/source-aliases.data", source=source,
                                 key=key, category="alias_missing_key"))
        elif isinstance(entry, dict) and rust_slots(source) != runtime_slots(entry.get("en-US", "")):
            findings.append(dict(file="locales/source-aliases.data", source=source,
                                 key=key, category="source_placeholder_mismatch"))
    return findings

def self_test() -> int:
    catalog = {"label": {locale: "Label" for locale in LOCALES},
               "template": {locale: "%{arg0} %{name}" for locale in LOCALES}}
    aliases = {"按钮": "label", "值 {} · {name}": "template"}
    cases = [
        ("direct", 'fn view(){button.label("按钮");}',
         lambda r: any(f["category"] == "untranslated_gui_literal" for f in r["findings"])),
        ("wrapped", 'fn view(){button.label(tr("按钮"));}',
         lambda r: not r["findings"] and len(r["used"]) == 1),
        ("conditional", 'fn view(){button.label(tr(if yes {"按钮"}else{"按钮"}));}',
         lambda r: not r["findings"]),
        ("embedded", 'fn view(){child(tf("值 {} · {name}",&[("arg0",format!("{}","按钮")),("name","raw".into())]));}',
         lambda r: any(f["category"] == "untranslated_embedded_argument" for f in r["findings"])),
        ("comment", '// child("按钮")\nfn view(){child("brand");}',
         lambda r: not r["used"]),
        ("tests", 'fn view(){child(tr("按钮"));}\n#[cfg(test)]mod tests{const RAW:&str="真实型号";}',
         lambda r: len(r["used"]) == 1),
        ("unicode", r'fn view(){child(tr("\u{6309}\u{94ae}"));}',
         lambda r: not r["findings"] and r["used"][0]["source"] == "按钮"),
        ("raw", 'fn view(){child(tr(r#"按钮"#));}',
         lambda r: not r["findings"]),
        ("typed_source", 'fn view(){message=msg("按钮");}',
         lambda r: not r["findings"] and len(r["used"]) == 1),
        ("typed_template", 'fn view(){message=mlf("值 {} · {name}",&[("arg0",raw.into()),("name",msg("按钮"))]);}',
         lambda r: not r["findings"]),
        ("captured_locale", 'fn view(){tf_in(locale,"值 {} · {name}",&[("arg0",raw),("name",trs_in(locale,"按钮"))]);}',
         lambda r: not r["findings"]),
        ("missing", 'fn view(){child(tr("新按钮"));}',
         lambda r: any(f["category"] == "catalog_missing" for f in r["findings"])),
        ("bad_args", 'fn view(){tf("值 {} · {name}",&[("arg0",raw)]);}',
         lambda r: any(f["category"] == "template_arguments" for f in r["findings"])),
        ("error_msg_is_not_ui_msg", 'fn view(){anyhow::Error::msg(tf("值 {} · {name}",&[("arg0",raw),("name",path)]));}',
         lambda r: not r["findings"] and len(r["used"]) == 1),
        ("opaque_raw", 'fn view(){tf("值 {} · {name}",&[("arg0",process.name.to_string()),("name",path.to_string())]);}',
         lambda r: not r["findings"]),
    ]
    for name, source, check in cases:
        result = audit_text("sample.rs", source, aliases, catalog)
        if not check(result):
            raise AssertionError("self-test failed: " + name + ": " + json.dumps(result, ensure_ascii=False))
    print(json.dumps(dict(self_tests_passed=len(cases), source_changes="none")))
    return 0

def load_json(path: Path) -> dict:
    def unique(pairs: list[tuple[str, object]]) -> dict:
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate JSON key: " + key + " in " + str(path))
            result[key] = value
        return result
    return json.loads(path.read_text(encoding="utf-8-sig"), object_pairs_hook=unique)

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--check", action="store_true", help="exit nonzero for findings requiring review")
    parser.add_argument("--self-test", action="store_true", help="run in-memory scanner fixtures")
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    root = args.root.resolve()
    catalogs = {name: load_json(root / name) for name in ("locales/app.json", "locales/framework.json")}
    aliases = load_json(root / "locales/source-aliases.data")
    findings = validate_catalogs(catalogs, aliases)
    used, exclusions, template_checks, hashes = [], [], [], {}
    files = sorted(p for p in (root / "src/ui").glob("*.rs")
                   if not p.name.startswith("qa") and p.name != "process_icons.rs")
    # Cross-file helpers are allowed only after checking their actual bodies.
    for helper_name, parameter in (("run_operation", "label"), ("confirm_management", "title")):
        for helper_path in files:
            helper_source = helper_path.read_text(encoding="utf-8-sig")
            if helper_translates(helper_source, model(helper_source), helper_name, parameter):
                SHARED_HELPERS.add(helper_name)
                break
    for path in files:
        source = path.read_text(encoding="utf-8-sig")
        hashes[path.relative_to(root).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
        result = audit_text(path.name, source, aliases, catalogs["locales/app.json"])
        findings.extend(result["findings"])
        used.extend(result["used"])
        exclusions.extend(result["exclusions"])
        template_checks.extend(result["template_checks"])
    backend_missing = []
    for filename, sources in BACKEND_INPUTS.items():
        source = (root / filename).read_text(encoding="utf-8-sig")
        test_start = source.find("#[cfg(test)]")
        if test_start >= 0:
            source = source[:test_start]
        tokens = tokenize(source)
        for literal in sources:
            token = next((t for t in tokens if t.kind == "string" and t.text == literal), None)
            if token is None:
                continue
            used.append(dict(source=literal, file=filename,
                             line=source.count("\n", 0, token.start) + 1,
                             reason="Known owned backend state/note presented by UI"))
            key = aliases.get(literal)
            entry = catalogs["locales/app.json"].get(key)
            if not key or not isinstance(entry, dict) or any(not entry.get(locale, "").strip() for locale in LOCALES):
                backend_missing.append(dict(file=filename, line=source.count("\n", 0, token.start) + 1,
                                            source=literal, key=key, owner="language_catalog",
                                            category="backend_catalog_missing"))
    literal_list = sorted({entry["source"] for entry in used})
    report = dict(
        generated_utc=datetime.now(timezone.utc).isoformat(),
        root=str(root), files=hashes,
        scope="Owned UI literal sources, narrow deferred presentation flows, literal template argument keys, and known backend states/notes",
        limitations=[
            "Lexical audit, not Rust type/dataflow analysis. Unreviewed findings need caller inspection.",
            "QA files, process_icons diagnostics, cfg(test) modules and comparison tokens are excluded.",
            "Process/device/user/signer/vendor names, paths and unknown external diagnostics stay verbatim.",
            "Variable sources/argument lists are deferred explicitly, not claimed verified.",
            "Known backend inventory is explicit; add new display-producing backend enums/notes when introducing features.",
            "Passing this audit does not prove rendered language switching, typography, native UI, accessibility or locale number/date formatting.",
        ],
        category_counts=dict(Counter(f["category"] for f in findings)),
        findings=findings, backend_catalog_missing=backend_missing,
        used_source_count=len(literal_list), used_sources=used,
        reviewed_exclusions=exclusions, template_checks=template_checks,
        core_test_contract=[
            "Regenerate used-sources.json during every verification; do not rely on a stale/manual list.",
            "Pass its Vec<String> converted to Vec<&str> to i18n::validate_used_sources to verify all aliases/locales/slots.",
            "Run this script --check separately: core catalog validation cannot detect unwrapped UI labels or opaque owned arguments.",
            "Keep new unsupported flows as findings until a narrow translated sink is demonstrated.",
        ],
    )
    out = root / ".cache/i18n-plan"
    out.mkdir(parents=True, exist_ok=True)
    for filename, value in (("ui-coverage-audit.json", report), ("used-sources.json", literal_list)):
        (out / filename).write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    rust_literals = [json.dumps(value, ensure_ascii=False).replace(r"\ufeff", r"\u{feff}").replace(r"\u0000", r"\0") for value in literal_list]
    (out / "used-sources.rs").write_text(
        "// Generated by scripts/verify-ui-localization.py; regenerate during verification.\n"
        "pub const OWNED_UI_SOURCES: &[&str] = &[\n" +
        "".join("    " + value + ",\n" for value in rust_literals) + "];\n",
        encoding="utf-8")
    print(json.dumps(dict(files=len(files), used_sources=len(literal_list),
                          catalog_keys=len(catalogs["locales/app.json"]) - 1,
                          framework_keys=len(catalogs["locales/framework.json"]) - 1,
                          category_counts=report["category_counts"],
                          backend_missing=len(backend_missing),
                          report=str(out / "ui-coverage-audit.json")), ensure_ascii=True, indent=2))
    return 1 if args.check and (findings or backend_missing) else 0

if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, AssertionError) as error:
        print("Localization audit failed: " + str(error), file=sys.stderr)
        sys.exit(2)
