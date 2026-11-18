// Characters by scene: one row per speaking character, one column per
// scene; a filled square where the character has dialogue in that scene.
// Drawn by hand in SVG (nothing loads from the internet). The writer's
// scene is shaded.
"use strict";

const S = Scriptorium;
const SVG = "http://www.w3.org/2000/svg";
const CELL = 14, GAP = 2, NAME_W = 160, COUNT_W = 40, TOP = 28;
const chart = document.getElementById("chart");
const status = document.getElementById("status");

let grid = null;  // { scenes: [heading], people: [{ name, scenes: Set, lines }], sceneOf: Map }
let here = null;  // the paragraph the writer is on
let hereRect = null;  // the shading on the writer's scene

function el(name, attrs, text) {
  const node = document.createElementNS(SVG, name);
  for (const [k, v] of Object.entries(attrs || {})) node.setAttribute(k, String(v));
  if (text !== undefined) node.textContent = text;  // script text: textContent only
  return node;
}

async function load() {
  // Every paragraph, in order: headings start scenes, and dialogue rows
  // carry their speaker.
  const rows = await S.paragraphs();
  const scenes = [];
  const sceneOf = new Map();
  const people = new Map();
  for (const row of rows) {
    if (row.kind === "Scene Heading") scenes.push(row.text);
    const at = scenes.length - 1;
    sceneOf.set(row.id, at);
    if (row.kind === "Dialogue" && row.speaker && at >= 0) {
      const p = people.get(row.speaker) || { name: row.speaker, scenes: new Set(), lines: 0 };
      p.scenes.add(at);
      p.lines += 1;
      people.set(row.speaker, p);
    }
  }
  const sorted = [...people.values()].sort((a, b) => b.lines - a.lines || a.name.localeCompare(b.name));
  grid = { scenes, people: sorted, sceneOf };
  draw();
}

function draw() {
  if (!grid) return;
  const { scenes, people, sceneOf } = grid;
  status.textContent = `${people.length} characters with dialogue, ${scenes.length} scenes`;
  const width = NAME_W + COUNT_W + scenes.length * (CELL + GAP);
  const height = TOP + people.length * (CELL + GAP);
  const svg = el("svg", { width, height, viewBox: `0 0 ${width} ${height}`, role: "img" });
  svg.append(el("title", {}, "Who speaks in which scene"));

  hereRect = el("rect", { class: "here", "pointer-events": "none", x: 0, y: 0, width: CELL + 2, height, visibility: "hidden" });
  // Scene numbers along the top, every fifth.
  scenes.forEach((heading, i) => {
    if (i === 0 || (i + 1) % 5 === 0) {
      svg.append(el("text", { x: NAME_W + COUNT_W + i * (CELL + GAP), y: TOP - 10 }, String(i + 1)));
    }
  });
  people.forEach((p, r) => {
    const y = TOP + r * (CELL + GAP);
    svg.append(el("text", { class: "name", x: 0, y: y + CELL - 3 }, p.name));
    svg.append(el("text", { class: "count", x: NAME_W, y: y + CELL - 3 }, String(p.scenes.size)));
    scenes.forEach((heading, i) => {
      const on = p.scenes.has(i);
      const cell = el("rect", { class: on ? "cell" : "empty", x: NAME_W + COUNT_W + i * (CELL + GAP), y, width: CELL, height: CELL, rx: 2 });
      cell.append(el("title", {}, `${p.name}${on ? " speaks" : " doesn't speak"} in scene ${i + 1}: ${heading}`));
      svg.append(cell);
    });
  });
  svg.append(hereRect);  // on top, see-through
  chart.replaceChildren(svg);
  markHere();
}

// Move the shading to the writer's scene, without drawing the grid again.
function markHere() {
  if (!grid || !hereRect) return;
  const at = here === null ? undefined : grid.sceneOf.get(here);
  if (at === undefined || at < 0) {
    hereRect.setAttribute("visibility", "hidden");
    return;
  }
  hereRect.setAttribute("x", String(NAME_W + COUNT_W + at * (CELL + GAP) - 1));
  hereRect.setAttribute("visibility", "visible");
}

// Called once with where the writer is now, then each time they move.
S.on("writer", (w) => { here = w.paragraph; markHere(); });
S.on("script", () => load());
load();
