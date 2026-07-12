#!/usr/bin/env python3
"""Audit clap Subcommand enums for resource-noun singular/plural alias gaps.

Only resource-noun command GROUPS are normalized — never verbs, positional
args, gerunds, or acronyms. The resource nouns are an explicit curated list
(PAIRS); algorithmic pluralization is deliberately avoided because it produces
garbage ("childrens", "bulk-sets", verb-plurals). Add a pair to extend.
"""
import re, sys, glob

# Curated singular<->plural resource-noun pairs. Either form may be canonical.
PAIRS = [
    ("agents","agent"),("keys","key"),("members","member"),("files","file"),
    ("jobs","job"),("integrations","integration"),("products","product"),
    ("orgs","org"),("bookings","booking"),("calendars","calendar"),
    ("types","type"),("blocks","block"),("contacts","contact"),
    ("companies","company"),("deals","deal"),("stages","stage"),
    ("tasks","task"),("conversations","conversation"),("invoices","invoice"),
    ("runs","run"),("cases","case"),("environments","environment"),
    ("deployments","deployment"),("schemas","schema"),("values","value"),
    ("operatives","operative"),("projects","project"),("widgets","widget"),
    ("skills","skill"),("providers","provider"),("plugins","plugin"),
    ("documents","document"),("notes","note"),("sources","source"),
    ("traces","trace"),("sourcemaps","sourcemap"),
]
FORM = {}
for a, b in PAIRS:
    FORM[a] = b
    FORM[b] = a

# (Enum, command) pairs intentionally NOT aliased for a SEMANTIC collision the
# auditor can't see (rule 4). Top-level `th agent` is agent-MESSAGING; the
# distinct `th api agents` is agent CRUD — aliasing `agent`->`agents` at the top
# level would make `th agents` mean messaging. Documented, deliberate.
SKIP_SEMANTIC = {("Commands", "agent")}

def kebab(ident):
    return re.sub(r'(?<!^)(?=[A-Z])', '-', ident).lower()

def parse_enums(src):
    out = []
    for m in re.finditer(r'#\[derive\([^)]*Subcommand[^)]*\)\][^\n]*\n(?:\s*#\[[^\]]*\]\s*\n)*\s*(?:pub )?enum (\w+)', src):
        name = m.group(1)
        i = src.index('{', m.end()-1)
        depth = 0; j = i
        while j < len(src):
            if src[j] == '{': depth += 1
            elif src[j] == '}':
                depth -= 1
                if depth == 0: break
            j += 1
        body = src[i+1:j]
        variants = []
        pending_name = None; pending_alias = []
        bdepth = 0; cur = None; cur_body = ""
        def flush():
            if cur:
                is_group = 'command(subcommand)' in cur_body
                variants.append((cur[0], cur[1], list(cur[2]), is_group))
        for line in body.splitlines():
            s = line.strip()
            # inside a struct-variant body: accumulate to detect nested subcommand,
            # and don't read FIELD names as variants
            if bdepth > 0:
                cur_body += line
                bdepth += line.count('{') - line.count('}')
                if bdepth == 0:
                    flush(); cur = None; cur_body = ""
                continue
            if s.startswith('///') or not s: continue
            if s.startswith('#['):
                nm = re.search(r'name\s*=\s*"([^"]+)"', s)
                if nm: pending_name = nm.group(1)
                for am in re.finditer(r'visible_alias(?:es)?\s*=\s*(?:"([^"]+)"|\[([^\]]*)\])', s):
                    if am.group(1): pending_alias.append(am.group(1))
                    if am.group(2): pending_alias += re.findall(r'"([^"]+)"', am.group(2))
                continue
            vm = re.match(r'([A-Z][A-Za-z0-9]*)\s*(\{|\(|,|$)', s)
            if vm:
                cur = (vm.group(1), pending_name, list(pending_alias)); cur_body = line
                pending_name = None; pending_alias = []
                bdepth += line.count('{') - line.count('}')
                if bdepth == 0:  # unit or tuple variant on one line — not a group
                    flush(); cur = None; cur_body = ""
        out.append((name, variants))
    return out

def main():
    files = sorted(glob.glob('crates/smooth-cli/src/**/*.rs', recursive=True))
    added = []
    for path in files:
        src = open(path).read()
        for enum, variants in parse_enums(src):
            names = {(nm or kebab(v)): al for v, nm, al, g in variants}
            for v, nm, al, is_group in variants:
                cmd = nm or kebab(v)
                if cmd not in FORM:
                    continue
                if not is_group:  # resource-noun COMMAND GROUPS only, never leaf verbs
                    continue
                want = FORM[cmd]
                have = want in al
                collide = want in names  # counterpart is a distinct command here
                if have:
                    status = "OK"
                elif collide:
                    status = "SKIP-collision"
                elif (enum, cmd) in SKIP_SEMANTIC:
                    status = "SKIP-semantic"
                else:
                    status = "GAP"
                print(f"{status:15} {path.split('/')[-1]:22} {enum:22} {cmd:14} -> {want}")
                if status == "GAP":
                    added.append((path, enum, cmd, want))
    print(f"\n=== {len(added)} gaps to add ===")

main()
