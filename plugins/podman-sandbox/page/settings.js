// The plugin's page for its settings in Moebius's web app: each Dockerfile in the web app's code editor, under a row of
// presets that replace its text. `src/page.rs` puts `PRESETS` before this text: by setting key, the presets of that
// setting, each a `label` and a `dockerfile`. Nothing is saved here; the box's Save button saves what `host.change`
// was last told.

export default function mount(element, host) {
  const values = { ...host.values };
  const takeDowns = [];
  const root = document.createElement("div");
  root.style.cssText = "display:flex;flex-direction:column;gap:24px;min-width:0";
  for (const setting of host.settings) {
    root.append(drawSetting(setting, values, host, takeDowns));
  }
  element.append(root);
  return () => {
    for (const takeDown of takeDowns) takeDown();
  };
}

function drawSetting(setting, values, host, takeDowns) {
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
      input.style.cssText =
        "min-height:40px;max-width:36rem;padding:0 12px;border:0;border-radius:var(--radius-control);" +
        "background:var(--field);color:var(--text);font:inherit";
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
      button.style.cssText =
        "min-height:32px;padding:0 12px;border:1px solid var(--hairline-strong);" +
        "border-radius:var(--radius-control);background:var(--surface-raised);color:var(--text);" +
        "font:inherit;font-size:var(--size-label);cursor:pointer";
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
