#!/usr/bin/env bun
import {
  mkdirSync,
  writeFileSync,
  readFileSync,
  existsSync,
  renameSync,
} from "fs";
import { join, resolve } from "path";
import { execSync } from "child_process";

type Phase = "candidate" | "extracted" | "specified" | "audited" | "integrated";

interface Concept {
  name: string;
  phase: Phase;
  source_files: string[];
  edges: {
    depends_on: string[];
    refines: string[];
    constrains: string[];
  };
  source_sha: string;
}

interface State {
  version: number;
  source_repo: string;
  concepts: Record<string, Concept>;
}

const VALID_PHASES: Phase[] = ["candidate", "extracted", "specified", "audited", "integrated"];
const VALID_EDGE_TYPES = ["depends_on", "refines", "constrains"];

const args = process.argv.slice(2);
const useJson = args.includes("--json");
const cleanArgs = args.filter((a) => a !== "--json");

function findProjectRoot(start: string): string {
  let dir = start;
  while (dir !== "/") {
    if (existsSync(join(dir, ".lat-reverse"))) return dir;
    const parent = resolve(dir, "..");
    if (parent === dir) break;
    dir = parent;
  }
  return start;
}

const projectRoot = findProjectRoot(process.cwd());
const latDir = join(projectRoot, ".lat-reverse");
const statePath = join(latDir, "state.json");
const conceptsDir = join(latDir, "concepts");

function output(data: unknown) {
  if (useJson) {
    console.log(JSON.stringify(data, null, 2));
  } else {
    console.log(data);
  }
}

function readState(): State {
  if (!existsSync(statePath)) {
    console.error("error: .lat-reverse/ not found. Run `lat-rev init` first.");
    process.exit(1);
  }
  return JSON.parse(readFileSync(statePath, "utf-8"));
}

function writeStateAtomic(state: State) {
  const tmp = statePath + ".tmp";
  writeFileSync(tmp, JSON.stringify(state, null, 2) + "\n");
  renameSync(tmp, statePath);
}

function gitHead(dir: string): string | null {
  try {
    return execSync("git rev-parse HEAD", { cwd: dir, encoding: "utf-8" }).trim();
  } catch {
    return null;
  }
}

function gitChangedFiles(dir: string, from: string, to: string, files: string[]): string[] {
  try {
    const result = execSync(`git diff --name-only ${from} ${to} -- ${files.join(" ")}`, {
      cwd: dir,
      encoding: "utf-8",
    }).trim();
    return result ? result.split("\n").filter(Boolean) : [];
  } catch {
    return files;
  }
}

function gitHasUncommitted(dir: string): boolean {
  try {
    const result = execSync("git status --porcelain", { cwd: dir, encoding: "utf-8" }).trim();
    return result.length > 0;
  } catch {
    return false;
  }
}

function gitListFiles(dir: string): string[] {
  try {
    const result = execSync("git ls-files", { cwd: dir, encoding: "utf-8" }).trim();
    return result ? result.split("\n").filter(Boolean) : [];
  } catch {
    return [];
  }
}

function nextStep(phase: Phase): string {
  switch (phase) {
    case "candidate":
      return "Next: /lat-rev-reconstruct <id> will run extraction phase";
    case "extracted":
      return "Next: /lat-rev-reconstruct <id> will run synthesis phase";
    case "specified":
      return "Next: /lat-rev-reconstruct <id> will run audit phase";
    case "audited":
      return "Next: /lat-rev-integrate <id>";
    case "integrated":
      return "Integrated. Run /lat-rev-drift to check for source changes.";
  }
}

// ---- Commands ----

function cmdInit() {
  const srcIdx = cleanArgs.indexOf("--src-dir");
  const srcDir = srcIdx !== -1 ? resolve(cleanArgs[srcIdx + 1] || ".") : ".";

  if (existsSync(latDir) && !cleanArgs.includes("--force")) {
    console.error("error: .lat-reverse/ already exists. Use --force to reinitialize.");
    process.exit(1);
  }

  mkdirSync(latDir, { recursive: true });
  mkdirSync(join(latDir, "bin"), { recursive: true });
  mkdirSync(conceptsDir, { recursive: true });

  const state: State = {
    version: 1,
    source_repo: srcDir,
    concepts: {},
  };
  writeStateAtomic(state);

  output({ ok: true, dir: latDir, source_repo: srcDir });
}

function cmdConceptEdge() {
  const conceptId = cleanArgs[2];
  const edgeType = cleanArgs[3];
  const targetId = cleanArgs[4];

  if (!conceptId || !edgeType || !targetId) {
    console.error("usage: lat-rev concept edge <id> <edge_type> <target_id>");
    console.error(`edge_type must be one of: ${VALID_EDGE_TYPES.join(", ")}`);
    process.exit(1);
  }

  if (!VALID_EDGE_TYPES.includes(edgeType)) {
    console.error(`error: invalid edge type "${edgeType}". Must be one of: ${VALID_EDGE_TYPES.join(", ")}`);
    process.exit(1);
  }

  const state = readState();

  if (!state.concepts[conceptId]) {
    console.error(`error: concept "${conceptId}" not found`);
    process.exit(1);
  }

  if (!state.concepts[targetId]) {
    console.error(`error: target concept "${targetId}" not found`);
    process.exit(1);
  }

  if (conceptId === targetId) {
    console.error("error: cannot create self-referential edge");
    process.exit(1);
  }

  const edges = state.concepts[conceptId].edges[edgeType as keyof Concept["edges"]];
  if (!edges.includes(targetId)) {
    edges.push(targetId);
    writeStateAtomic(state);
  }

  output({ ok: true, concept: conceptId, edge: edgeType, target: targetId });
}

function cmdStatus() {
  const state = readState();

  const concepts: Array<{ id: string; name: string; phase: string; stale: boolean; changed_files: string[] }> = [];
  const counts: Record<string, number> = {};
  for (const p of VALID_PHASES) counts[p] = 0;
  const stale: Array<{ id: string; changed_files: string[] }> = [];

  const head = gitHead(state.source_repo);
  for (const [id, c] of Object.entries(state.concepts)) {
    counts[c.phase] = (counts[c.phase] || 0) + 1;
    const isStale = !!head && !!c.source_sha && head !== c.source_sha;
    const changedFiles = isStale && head
      ? gitChangedFiles(state.source_repo, c.source_sha, head, c.source_files)
      : [];
    concepts.push({ id, name: c.name, phase: c.phase, stale: !!isStale, changed_files: changedFiles });
    if (isStale) {
      stale.push({ id, changed_files: changedFiles });
    }
  }

  const phaseStr = VALID_PHASES.map((p) => `${p}(${counts[p]})`).join(" ");
  const staleStr =
    stale.length > 0
      ? stale.map((s) => `${s.id} (${s.changed_files.join(", ") || "unknown"})`).join(", ")
      : "none";

  const firstCandidate = concepts.find((c) => c.phase === "candidate");
  const firstExtracted = concepts.find((c) => c.phase === "extracted");
  const firstSpecified = concepts.find((c) => c.phase === "specified");
  const firstUnaudited = firstCandidate || firstExtracted || firstSpecified;

  const recommendation = stale.length > 0
    ? `Recommended: /lat-rev-reconstruct ${stale[0].id} (source changed since last extraction)`
    : firstUnaudited
    ? `Recommended: /lat-rev-reconstruct ${firstUnaudited.id} (${firstUnaudited.phase})`
    : counts["audited"] > 0
    ? "All concepts audited. /lat-rev-integrate to write to lat.md/"
    : "No concepts yet. /lat-rev-split to decompose the codebase";

  if (useJson) {
    output({ phases: counts, concepts, stale, recommendation });
  } else {
    console.log(`Phase: ${phaseStr}`);
    for (const c of concepts) {
      const staleMarker = c.stale ? " STALE" : "";
      console.log(`  ${c.id} → ${c.phase}${staleMarker} (${c.name})`);
    }
    if (stale.length > 0) console.log(`${stale.length} concepts STALE: ${staleStr}`);
    console.log(recommendation);
  }
}

function cmdDrift() {
  const state = readState();
  const conceptId = cleanArgs[1];
  const head = gitHead(state.source_repo);

  if (!head) {
    console.error("error: not a git repository or no commits");
    process.exit(1);
  }

  if (conceptId) {
    const concept = state.concepts[conceptId];
    if (!concept) {
      console.error(`error: concept "${conceptId}" not found`);
      process.exit(1);
    }

    if (!concept.source_sha) {
      output({ id: conceptId, stale: false, changed_files: [], note: "no snapshot recorded" });
      return;
    }

    const isStale = head !== concept.source_sha;
    const changedFiles = isStale
      ? gitChangedFiles(state.source_repo, concept.source_sha, head, concept.source_files)
      : [];

    output({ id: conceptId, stale: isStale, old_sha: concept.source_sha, current_sha: head, changed_files: changedFiles });
    return;
  }

  const results: Array<{
    id: string;
    stale: boolean;
    changed_files: string[];
  }> = [];

  for (const [id, c] of Object.entries(state.concepts)) {
    if (!c.source_sha) {
      results.push({ id, stale: false, changed_files: [] });
      continue;
    }
    const isStale = head !== c.source_sha;
    const changedFiles = isStale
      ? gitChangedFiles(state.source_repo, c.source_sha, head, c.source_files)
      : [];
    results.push({ id, stale: isStale, changed_files: changedFiles });
  }

  if (useJson) {
    output({ current_sha: head, concepts: results });
  } else {
    for (const r of results) {
      if (r.stale) {
        console.log(`STALE: ${r.id} — ${r.changed_files.join(", ") || "unknown changes"}`);
      }
    }
    const staleCount = results.filter((r) => r.stale).length;
    if (staleCount === 0) console.log("All concepts up to date.");
  }
}

function cmdSnapshot() {
  if (cleanArgs.includes("--all")) {
    cmdSnapshotAll();
    return;
  }

  const conceptId = cleanArgs[1];
  if (!conceptId) {
    console.error("usage: lat-rev snapshot <concept_id>  OR  lat-rev snapshot --all [--phase <phase>]");
    process.exit(1);
  }

  const state = readState();
  const concept = state.concepts[conceptId];
  if (!concept) {
    console.error(`error: concept "${conceptId}" not found`);
    process.exit(1);
  }

  if (gitHasUncommitted(state.source_repo)) {
    console.warn("warning: uncommitted changes exist in source repo. Snapshot may be inaccurate.");
  }

  const head = gitHead(state.source_repo);
  if (!head) {
    console.error("error: cannot get git HEAD");
    process.exit(1);
  }

  concept.source_sha = head;
  writeStateAtomic(state);

  output({ ok: true, concept: conceptId, source_sha: head });
}

function cmdSnapshotAll() {
  const phaseIdx = cleanArgs.indexOf("--phase");
  const phaseFilter = phaseIdx !== -1 ? cleanArgs[phaseIdx + 1] as Phase : undefined;

  if (phaseFilter && !VALID_PHASES.includes(phaseFilter)) {
    console.error(`error: --phase must be one of: ${VALID_PHASES.join(", ")}`);
    process.exit(1);
  }

  const state = readState();

  if (gitHasUncommitted(state.source_repo)) {
    console.warn("warning: uncommitted changes exist in source repo. Snapshot may be inaccurate.");
  }

  const head = gitHead(state.source_repo);
  if (!head) {
    console.error("error: cannot get git HEAD");
    process.exit(1);
  }

  let count = 0;
  for (const [id, concept] of Object.entries(state.concepts)) {
    if (phaseFilter && concept.phase !== phaseFilter) continue;
    concept.source_sha = head;
    count++;
  }

  writeStateAtomic(state);
  output({ ok: true, snapshotted: count, source_sha: head });
}

function cmdConceptAdd() {
  const conceptId = cleanArgs[2];
  const nameIdx = cleanArgs.indexOf("--name");
  const filesIdx = cleanArgs.indexOf("--files");

  if (!conceptId || nameIdx === -1 || filesIdx === -1) {
    console.error("usage: lat-rev concept add <id> --name <name> --files <f1,f2,...>");
    process.exit(1);
  }

  const name = cleanArgs[nameIdx + 1];
  const files = cleanArgs[filesIdx + 1]?.split(",").filter(Boolean) || [];

  if (!name) {
    console.error("error: --name requires a value");
    process.exit(1);
  }

  const state = readState();

  if (state.concepts[conceptId]) {
    console.error(`error: concept \"${conceptId}\" already exists`);
    process.exit(1);
  }

  state.concepts[conceptId] = {
    name,
    phase: "candidate",
    source_files: files,
    edges: { depends_on: [], refines: [], constrains: [] },
    source_sha: "",
  };
  writeStateAtomic(state);

  output({ ok: true, id: conceptId, name, source_files: files });
}

function cmdConceptAddBatch() {
  const fileIdx = cleanArgs.indexOf("--file");
  if (fileIdx === -1) {
    console.error("usage: lat-rev concept add-batch --file <path|->");
    console.error("  JSON format: [{\"id\": \"...\", \"name\": \"...\", \"source_files\": [...]}]");
    console.error("  Use --file - to read from stdin");
    process.exit(1);
  }

  const filePath = cleanArgs[fileIdx + 1];
  let input: string;

  if (filePath === "-") {
    input = readFileSync("/dev/stdin", "utf-8");
  } else {
    if (!existsSync(filePath)) {
      console.error(`error: file not found: ${filePath}`);
      process.exit(1);
    }
    input = readFileSync(filePath, "utf-8");
  }

  let entries: Array<{ id: string; name: string; source_files: string[] }>;
  try {
    entries = JSON.parse(input);
  } catch {
    console.error("error: invalid JSON input");
    process.exit(1);
  }

  if (!Array.isArray(entries) || entries.length === 0) {
    console.error("error: input must be a non-empty JSON array");
    process.exit(1);
  }

  for (const entry of entries) {
    if (!entry.id || !entry.name || !Array.isArray(entry.source_files)) {
      console.error(`error: each entry must have id, name, and source_files (array). Failed on: ${JSON.stringify(entry)}`);
      process.exit(1);
    }
  }

  const state = readState();

  for (const entry of entries) {
    if (state.concepts[entry.id]) {
      console.error(`error: concept "${entry.id}" already exists`);
      process.exit(1);
    }
  }

  for (const entry of entries) {
    state.concepts[entry.id] = {
      name: entry.name,
      phase: "candidate",
      source_files: entry.source_files,
      edges: { depends_on: [], refines: [], constrains: [] },
      source_sha: "",
    };
  }
  writeStateAtomic(state);

  output({ ok: true, added: entries.length, ids: entries.map((e) => e.id) });
}

function cmdConceptPromote() {
  const conceptId = cleanArgs[2];
  const phaseIdx = cleanArgs.indexOf("--phase");

  if (!conceptId || phaseIdx === -1) {
    console.error("usage: lat-rev concept promote <id> --phase <extracted|specified|audited|integrated>");
    process.exit(1);
  }

  const phase = cleanArgs[phaseIdx + 1];
  if (!phase || !VALID_PHASES.includes(phase as Phase)) {
    console.error(`error: --phase must be one of: ${VALID_PHASES.join(", ")}`);
    process.exit(1);
  }

  const state = readState();
  const concept = state.concepts[conceptId];
  if (!concept) {
    console.error(`error: concept "${conceptId}" not found`);
    process.exit(1);
  }

  const currentIdx = VALID_PHASES.indexOf(concept.phase);
  const newIdx = VALID_PHASES.indexOf(phase as Phase);
  if (newIdx !== currentIdx + 1) {
    console.error(`error: cannot promote from ${concept.phase} to ${phase}. Must advance one step at a time: ${VALID_PHASES.slice(currentIdx + 1).join(" → ")}`);
    process.exit(1);
  }

  concept.phase = phase as Phase;
  writeStateAtomic(state);

  output({ ok: true, id: conceptId, phase: concept.phase });
}

function cmdConceptPromoteBatch() {
  const fromIdx = cleanArgs.indexOf("--from");
  const toIdx = cleanArgs.indexOf("--to");

  if (fromIdx === -1 || toIdx === -1) {
    console.error("usage: lat-rev concept promote-batch --from <phase> --to <phase>");
    process.exit(1);
  }

  const fromPhase = cleanArgs[fromIdx + 1] as Phase;
  const toPhase = cleanArgs[toIdx + 1] as Phase;

  if (!VALID_PHASES.includes(fromPhase)) {
    console.error(`error: --from must be one of: ${VALID_PHASES.join(", ")}`);
    process.exit(1);
  }

  if (!VALID_PHASES.includes(toPhase)) {
    console.error(`error: --to must be one of: ${VALID_PHASES.join(", ")}`);
    process.exit(1);
  }

  const fromIdx2 = VALID_PHASES.indexOf(fromPhase);
  const toIdx2 = VALID_PHASES.indexOf(toPhase);
  if (toIdx2 !== fromIdx2 + 1) {
    console.error(`error: cannot promote from ${fromPhase} to ${toPhase}. Must advance one step: ${fromPhase} → ${VALID_PHASES[fromIdx2 + 1]}`);
    process.exit(1);
  }

  const state = readState();
  const toPromote = Object.entries(state.concepts).filter(([_, c]) => c.phase === fromPhase);

  if (toPromote.length === 0) {
    console.error(`error: no concepts at phase "${fromPhase}"`);
    process.exit(1);
  }

  for (const [id, concept] of toPromote) {
    concept.phase = toPhase;
  }
  writeStateAtomic(state);

  output({ ok: true, promoted: toPromote.length, from: fromPhase, to: toPhase, ids: toPromote.map(([id]) => id) });
}

function cmdConceptReset() {
  const conceptId = cleanArgs[2];

  if (!conceptId) {
    console.error("usage: lat-rev concept reset <id>");
    process.exit(1);
  }

  const state = readState();
  const concept = state.concepts[conceptId];
  if (!concept) {
    console.error(`error: concept "${conceptId}" not found`);
    process.exit(1);
  }

  concept.phase = "candidate";
  concept.source_sha = "";
  writeStateAtomic(state);

  output({ ok: true, id: conceptId, phase: concept.phase });
}

function cmdConceptShow() {
  const conceptId = cleanArgs[2];

  if (!conceptId) {
    console.error("usage: lat-rev concept show <id>");
    process.exit(1);
  }

  const state = readState();
  const concept = state.concepts[conceptId];
  if (!concept) {
    console.error(`error: concept "${conceptId}" not found`);
    process.exit(1);
  }

  const head = gitHead(state.source_repo);
  const isStale = !!head && !!concept.source_sha && head !== concept.source_sha;
  const changedFiles =
    isStale && head && concept.source_files.length > 0
      ? gitChangedFiles(state.source_repo, concept.source_sha, head, concept.source_files)
      : [];

  output({
    id: conceptId,
    name: concept.name,
    phase: concept.phase,
    source_files: concept.source_files,
    edges: concept.edges,
    source_sha: concept.source_sha,
    stale: !!isStale,
    changed_files: changedFiles,
  });
}

function cmdConceptList() {
  const phaseIdx = cleanArgs.indexOf("--phase");
  const phaseFilter = phaseIdx !== -1 ? cleanArgs[phaseIdx + 1] as Phase : undefined;

  if (phaseFilter && !VALID_PHASES.includes(phaseFilter)) {
    console.error(`error: --phase must be one of: ${VALID_PHASES.join(", ")}`);
    process.exit(1);
  }

  const state = readState();
  const entries = Object.entries(state.concepts)
    .filter(([_, c]) => !phaseFilter || c.phase === phaseFilter)
    .map(([id, c]) => ({ id, name: c.name, phase: c.phase }));

  if (useJson) {
    output({ count: entries.length, concepts: entries });
  } else {
    if (entries.length === 0) {
      console.log(phaseFilter ? `No concepts at phase "${phaseFilter}".` : "No concepts yet.");
      return;
    }
    for (const e of entries) {
      console.log(`  ${e.id} → ${e.phase} (${e.name})`);
    }
  }
}

function cmdConceptNext() {
  const countIdx = cleanArgs.indexOf("--count");
  const count = countIdx !== -1 ? parseInt(cleanArgs[countIdx + 1], 10) || 5 : 5;

  const state = readState();
  const head = gitHead(state.source_repo);

  const scored = Object.entries(state.concepts).map(([id, c]) => {
    const isStale = !!head && !!c.source_sha && head !== c.source_sha;
    const phaseIdx = VALID_PHASES.indexOf(c.phase);
    let priority = phaseIdx;
    if (isStale) priority += VALID_PHASES.length;
    return { id, name: c.name, phase: c.phase, stale: !!isStale, priority };
  });

  scored.sort((a, b) => b.priority - a.priority);

  const next = scored.slice(0, count);

  if (useJson) {
    output({ count: next.length, concepts: next.map(({ priority, ...rest }) => rest) });
  } else {
    if (next.length === 0) {
      console.log("No concepts to process.");
      return;
    }
    for (const n of next) {
      const staleMarker = n.stale ? " STALE" : "";
      console.log(`  ${n.id} → ${n.phase}${staleMarker} (${n.name})`);
    }
  }
}

function cmdConceptCoverage() {
  const state = readState();

  const allFiles = gitListFiles(state.source_repo);
  if (allFiles.length === 0) {
    console.error("error: no tracked files found in source repo");
    process.exit(1);
  }

  const covered = new Set<string>();
  for (const concept of Object.values(state.concepts)) {
    for (const f of concept.source_files) {
      covered.add(f);
    }
  }

  const uncovered = allFiles.filter((f) => !covered.has(f));

  if (useJson) {
    output({ total: allFiles.length, covered: covered.size, uncovered });
  } else {
    if (uncovered.length === 0) {
      console.log(`All ${allFiles.length} files covered by concepts.`);
    } else {
      console.log(`${uncovered.length} uncovered files (of ${allFiles.length}):`);
      for (const f of uncovered) {
        console.log(`  ${f}`);
      }
    }
  }
}

// ---- Dispatch ----

const command = cleanArgs[0];

switch (command) {
  case "init":
    cmdInit();
    break;
  case 'concept': {
    const sub = cleanArgs[1];
    if (sub === 'edge') {
      cmdConceptEdge();
    } else if (sub === 'add') {
      cmdConceptAdd();
    } else if (sub === 'add-batch') {
      cmdConceptAddBatch();
    } else if (sub === 'promote') {
      cmdConceptPromote();
    } else if (sub === 'promote-batch') {
      cmdConceptPromoteBatch();
    } else if (sub === 'reset') {
      cmdConceptReset();
    } else if (sub === 'show') {
      cmdConceptShow();
    } else if (sub === 'list') {
      cmdConceptList();
    } else if (sub === 'next') {
      cmdConceptNext();
    } else if (sub === 'coverage') {
      cmdConceptCoverage();
    } else {
      console.error('usage: lat-rev concept <add|add-batch|edge|promote|promote-batch|reset|show|list|next|coverage> ...');
      process.exit(1);
    }
    break;
  }
  case "status":
    cmdStatus();
    break;
  case "drift":
    cmdDrift();
    break;
  case "snapshot":
    cmdSnapshot();
    break;
  default:
    console.log(`lat-rev — LAT reconstruction state manager

Usage:
  lat-rev init [--src-dir <path>] [--force]
  lat-rev concept add <id> --name <name> --files <f1,f2,...>
  lat-rev concept add-batch --file <path|->   (JSON array on stdin if --file -)
  lat-rev concept promote <id> --phase <extracted|specified|audited|integrated>
  lat-rev concept promote-batch --from <phase> --to <phase>
  lat-rev concept reset <id>
  lat-rev concept show <id>
  lat-rev concept edge <id> <edge_type> <target_id>
  lat-rev concept list [--phase <phase>]
  lat-rev concept next [--count N]
  lat-rev concept coverage
  lat-rev status
  lat-rev drift [<concept_id>]
  lat-rev snapshot <concept_id>
  lat-rev snapshot --all [--phase <phase>]

Concept lifecycle: candidate → extracted → specified → audited → integrated
Edge types: depends_on, refines, constrains

Global options:
  --json    machine-readable output`);
}
