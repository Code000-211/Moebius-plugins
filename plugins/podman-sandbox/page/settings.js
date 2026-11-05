// The plugin's page for its settings in Moebius's web app: each Dockerfile in the web app's code editor, under a row of
// presets that replace its text, and a repository's mounts as a table, one row a line of the setting. `src/page.rs` puts `PRESETS` before this text: by setting key, the presets of that
// setting, each a `label` and a `dockerfile`. Nothing is saved here; the box's Save button saves what `host.change`
// was last told.

export default function mount(element, host) {
  const values = { ...host.values };
  const takeDowns = [];
  const root = document.createElement("div");
  root.style.cssText = "display:flex;flex-direction:column;gap:24px;min-width:0";
  for (const setting of host.settings) {
    root.append(
      setting.key === "mounts"
        ? drawMounts(setting, values, host)
        : drawSetting(setting, values, host, takeDowns),
    );
  }
  element.append(root);
  return () => {
    for (const takeDown of takeDowns) takeDown();
  };
}

const FIELD =
  "min-height:40px;min-width:0;padding:0 12px;border:0;border-radius:var(--radius-control);" +
  "background:var(--field);color:var(--text);font:inherit";
const BUTTON =
  "min-height:32px;padding:0 12px;border:1px solid var(--hairline-strong);" +
  "border-radius:var(--radius-control);background:var(--surface-raised);color:var(--text);" +
  "font:inherit;font-size:var(--size-label);cursor:pointer";

/** A setting's box with its label and description above what `drawSetting` or `drawMounts` puts in it. */
function settingBox(setting) {
  const box = document.createElement("div");
  box.style.cssText = "display:flex;flex-direction:column;gap:8px;min-width:0";
  const label = document.createElement("span");
  label.textContent = setting.label;
  label.style.cssText =
    "font-size:var(--size-label);font-weight:600;color:var(--text-secondary)";
  box.append(label);
  if (setting.description) {
    const description = document.createElement("span");
    description.textContent = setting.description;
    description.style.cssText =
      "max-width:65ch;font-size:var(--size-label);color:var(--text-secondary)";
    box.append(description);
  }
  return box;
}

function drawSetting(setting, values, host, takeDowns) {
  const box = settingBox(setting);

  const editor = document.createElement("div");
  editor.style.minWidth = "0";
  const slot = { takeDown: () => {} };
  takeDowns.push(() => slot.takeDown());
  const set = (text) => {
    values[setting.key] = text;
    host.change({ ...values });
  };
  const draw = (text) => {
    slot.takeDown();
    if (setting.input === "code") {
      slot.takeDown = host.codeEditor(editor, {
        label: setting.label,
        language: setting.language,
        value: text,
        onChange: set,
      });
    } else {
      const input = document.createElement("input");
      input.value = text;
      input.setAttribute("aria-label", setting.label);
      input.autocomplete = "off";
      input.style.cssText = `${FIELD};max-width:36rem`;
      input.oninput = () => set(input.value);
      editor.replaceChildren(input);
      slot.takeDown = () => editor.replaceChildren();
    }
  };

  const presets = PRESETS[setting.key] ?? [];
  if (presets.length > 0) {
    const row = document.createElement("div");
    row.setAttribute("role", "group");
    row.setAttribute("aria-label", `Presets of ${setting.label}`);
    row.style.cssText = "display:flex;flex-wrap:wrap;align-items:center;gap:8px";
    const lead = document.createElement("span");
    lead.textContent = "Start from";
    lead.style.cssText = "font-size:var(--size-label);color:var(--text-secondary)";
    row.append(lead);
    for (const preset of presets) {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = preset.label;
      button.style.cssText = BUTTON;
      button.onclick = () => {
        set(preset.dockerfile);
        draw(preset.dockerfile);
      };
      row.append(button);
    }
    box.append(row);
  }

  box.append(editor);
  draw(values[setting.key] ?? "");
  return box;
}

/**
 * The mounts of `text`, one a line: `<host path>:<container path>[:ro|:rw]` or
 * `volume:<name>:<container path>[:ro|:rw]`. A comment or a line that is not a mount is kept as written, so a save
 * from the table never loses it; the plugin says what is wrong with it when a run starts.
 */
export function parseMounts(text) {
  const rows = [];
  const kept = [];
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (line === "") continue;
    let parts = line.split(":");
    const volume = parts[0] === "volume";
    if (volume) parts = parts.slice(1);
    let writable = volume;
    if (parts.length === 3 && (parts[2] === "ro" || parts[2] === "rw")) {
      writable = parts[2] === "rw";
      parts = parts.slice(0, 2);
    }
    if (line.startsWith("#") || parts.length !== 2) {
      kept.push(line);
      continue;
    }
    rows.push({ volume, source: parts[0], target: parts[1], writable });
  }
  return { rows, kept };
}

/** The text of the setting: the kept lines, then a line for each row that has something in it. */
export function writeMounts({ rows, kept }) {
  const lines = rows
    .filter((row) => row.source.trim() !== "" || row.target.trim() !== "")
    .map((row) => {
      const mode = row.writable === row.volume ? "" : row.writable ? ":rw" : ":ro";
      return `${row.volume ? "volume:" : ""}${row.source.trim()}:${row.target.trim()}${mode}`;
    });
  const all = [...kept, ...lines];
  return all.length === 0 ? "" : `${all.join("\n")}\n`;
}

function select(label, options, value, onChange) {
  const element = document.createElement("select");
  element.setAttribute("aria-label", label);
  element.style.cssText = FIELD;
  for (const [optionValue, text] of options) {
    const option = document.createElement("option");
    option.value = optionValue;
    option.textContent = text;
    element.append(option);
  }
  element.value = value;
  element.onchange = () => onChange(element.value);
  return element;
}

function drawMounts(setting, values, host) {
  const box = settingBox(setting);
  const state = parseMounts(values[setting.key] ?? "");
  const report = () => {
    values[setting.key] = writeMounts(state);
    host.change({ ...values });
  };
  const table = document.createElement("div");
  table.setAttribute("role", "group");
  table.setAttribute("aria-label", setting.label);
  table.style.cssText = "display:flex;flex-direction:column;gap:8px;min-width:0";

  const draw = () => {
    table.replaceChildren();
    if (state.kept.length > 0) {
      const note = document.createElement("span");
      note.textContent = `Kept as written: ${state.kept.join(" · ")}`;
      note.style.cssText = "font-size:var(--size-label);color:var(--text-secondary)";
      table.append(note);
    }
    state.rows.forEach((row, index) => {
      const n = index + 1;
      const line = document.createElement("div");
      line.style.cssText =
        "display:grid;grid-template-columns:9rem minmax(0,1fr) minmax(0,1fr) 11rem auto;gap:8px;align-items:center";
      const source = document.createElement("input");
      const target = document.createElement("input");
      for (const [input, key, label] of [
        [source, "source", `Source of mount ${n}`],
        [target, "target", `Container path of mount ${n}`],
      ]) {
        input.value = row[key];
        input.autocomplete = "off";
        input.setAttribute("aria-label", label);
        input.style.cssText = FIELD;
        input.oninput = () => {
          row[key] = input.value;
          report();
        };
      }
      source.placeholder = row.volume ? "shop-cargo" : "/srv/fixtures";
      target.placeholder = row.volume ? "/home/moebius/.cargo" : "/fixtures";
      const kind = select(
        `Kind of mount ${n}`,
        [
          ["folder", "Host folder"],
          ["volume", "Volume"],
        ],
        row.volume ? "volume" : "folder",
        (value) => {
          row.volume = value === "volume";
          row.writable = row.volume;
          report();
          draw();
        },
      );
      const mode = select(
        `Access of mount ${n}`,
        [
          ["ro", "Read only"],
          ["rw", "Read and write"],
        ],
        row.writable ? "rw" : "ro",
        (value) => {
          row.writable = value === "rw";
          report();
        },
      );
      const remove = document.createElement("button");
      remove.type = "button";
      remove.textContent = "Remove";
      remove.setAttribute("aria-label", `Remove mount ${n}`);
      remove.style.cssText = BUTTON;
      remove.onclick = () => {
        state.rows.splice(index, 1);
        report();
        draw();
      };
      line.append(kind, source, target, mode, remove);
      table.append(line);
    });
    const add = document.createElement("button");
    add.type = "button";
    add.textContent = "Add a mount";
    add.style.cssText = `${BUTTON};align-self:flex-start`;
    add.onclick = () => {
      state.rows.push({ volume: false, source: "", target: "", writable: false });
      draw();
      table.querySelectorAll("input")[state.rows.length * 2 - 2]?.focus();
    };
    table.append(add);
  };
  draw();
  box.append(table);
  return box;
}
