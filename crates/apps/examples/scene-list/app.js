// Scene list: every scene of the open script, the writer's scene marked.
// Click a scene to suggest a new heading for it (one line, into the
// default review, "scene-list (app).review.txt").
"use strict";

const S = Scriptorium;
const list = document.getElementById("scenes");
const status = document.getElementById("status");
const edit = document.getElementById("edit");
const editText = document.getElementById("edit-text");
const editResult = document.getElementById("edit-result");

let scenes = [];          // [{ number, id, heading, lines }]
let sceneOf = new Map();  // paragraph id -> index into scenes
let here = null;          // the paragraph the writer is on
let editing = null;       // the scene being edited

async function load() {
  const rows = await S.paragraphs();
  scenes = [];
  sceneOf = new Map();
  for (const row of rows) {
    if (row.kind === "Scene Heading") {
      scenes.push({ number: scenes.length + 1, id: row.id, heading: row.text, lines: 0 });
    }
    if (scenes.length > 0) {
      scenes[scenes.length - 1].lines += 1;
      sceneOf.set(row.id, scenes.length - 1);
    }
  }
  render();
}

function render() {
  const items = scenes.map((scene, i) => {
    const li = document.createElement("li");
    li.dataset.index = String(i);
    const number = document.createElement("span");
    number.className = "number";
    number.textContent = String(scene.number);
    const heading = document.createElement("span");
    heading.className = "heading";
    heading.textContent = scene.heading || "(empty heading)";  // script text: textContent only
    const lines = document.createElement("span");
    lines.className = "lines";
    lines.textContent = `${scene.lines} lines`;
    li.append(number, heading, lines);
    li.addEventListener("click", () => startEdit(i));
    return li;
  });
  list.replaceChildren(...items);
  status.textContent = `${scenes.length} scenes`;
  mark(false);
}

// Mark the writer's scene, and scroll to it when they move.
function mark(scroll) {
  const at = here === null ? undefined : sceneOf.get(here);
  for (const li of list.children) {
    const on = Number(li.dataset.index) === at;
    li.classList.toggle("here", on);
    if (on && scroll) li.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }
}

function startEdit(i) {
  editing = scenes[i];
  document.getElementById("edit-number").textContent = String(editing.number);
  editText.value = editing.heading;
  editResult.textContent = "";
  edit.hidden = false;
  editText.focus();
}

document.getElementById("edit-cancel").addEventListener("click", () => {
  edit.hidden = true;
  editing = null;
});

document.getElementById("edit-form").addEventListener("submit", async (e) => {
  e.preventDefault();  // never post the form: the library makes the call
  if (!editing) return;
  const text = editText.value.trim();
  if (text === "" || text === editing.heading) {
    editResult.textContent = "Change the heading first.";
    return;
  }
  editResult.textContent = "Suggesting…";
  try {
    const answer = await S.suggest([{ paragraph_id: editing.id, options: [text] }]);
    editResult.textContent = answer.failed.length > 0
      ? `Not suggested: ${answer.failed[0].error}`
      : "Suggested. The writer decides it in Scriptorium.";
  } catch (err) {
    editResult.textContent = err.message;
  }
});

// Called once with where the writer is now, then each time they move.
S.on("writer", (w) => { here = w.paragraph; mark(true); });
S.on("script", () => load());
load();
