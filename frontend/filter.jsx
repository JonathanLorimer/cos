import { render } from "preact";
import { signal, effect } from "@preact/signals";

function Filter({ tableId }) {
  const query = signal("");
  const shown = signal(0);

  effect(() => {
    const table = document.getElementById(tableId);
    if (!table) return;
    const rows = table.querySelectorAll("tbody tr");
    const needle = query.value.trim().toLowerCase();
    let count = 0;
    rows.forEach((row) => {
      const haystack = (row.dataset.search || "").toLowerCase();
      const match = needle === "" || haystack.includes(needle);
      row.classList.toggle("hidden", !match);
      if (match) count += 1;
    });
    shown.value = count;
  });

  return (
    <div class="flex items-center gap-3 mb-3">
      <input
        type="text"
        class="border border-slate-300 rounded px-2 py-1 text-sm w-72 focus:outline-none focus:ring-2 focus:ring-slate-400"
        placeholder="Filter rows..."
        onInput={(e) => {
          query.value = e.currentTarget.value;
        }}
      />
      <span class="text-xs text-slate-500">{shown.value} shown</span>
    </div>
  );
}

document.querySelectorAll('[data-island="filter"]').forEach((el) => {
  render(<Filter tableId={el.dataset.table} />, el);
});
