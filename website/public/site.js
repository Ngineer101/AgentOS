// Adds a Copy button to every code block. Prompts and comments are left out of what is copied.
for (const pre of document.querySelectorAll("pre")) {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "copy";
  btn.textContent = "Copy";
  btn.addEventListener("click", async () => {
    const clone = pre.cloneNode(true);
    clone.querySelectorAll(".copy, .p, .c").forEach((n) => n.remove());
    const text = clone.textContent.split("\n").map((l) => l.trimEnd()).join("\n").trim();
    try {
      await navigator.clipboard.writeText(text);
      btn.textContent = "Copied";
    } catch {
      btn.textContent = "Press Ctrl+C";
    }
    setTimeout(() => (btn.textContent = "Copy"), 1600);
  });
  pre.append(btn);
}
