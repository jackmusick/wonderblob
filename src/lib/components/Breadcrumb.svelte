<script lang="ts">
  import { activeConnection, currentPath } from "../stores/session";

  const segments = $derived.by(() => {
    const path = $currentPath;
    const parts = path.split("/").filter((p) => p.length > 0);
    let acc = "";
    return parts.map((name) => {
      acc += `/${name}`;
      return { name, path: acc };
    });
  });

  function navigate(path: string) {
    if (path !== $currentPath) currentPath.set(path);
  }
</script>

<nav class="breadcrumb" aria-label="Path">
  <button
    class="segment root"
    class:current={segments.length === 0}
    title="/"
    onclick={() => navigate("/")}
  >
    {$activeConnection?.bookmark.label ?? "/"}
  </button>
  {#each segments as seg, i (seg.path)}
    <span class="sep" aria-hidden="true">/</span>
    <button
      class="segment"
      class:current={i === segments.length - 1}
      title={seg.path}
      onclick={() => navigate(seg.path)}
    >
      {seg.name}
    </button>
  {/each}
</nav>

<style>
  .breadcrumb {
    display: flex;
    align-items: center;
    gap: 1px;
    min-width: 0;
    overflow: hidden;
    flex: 1;
  }
  .segment {
    flex-shrink: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    height: 22px;
    padding: 0 6px;
    font-size: var(--text-base);
    font-family: var(--font-ui);
    color: var(--fg-secondary);
    background: transparent;
    border: none;
    border-radius: var(--radius);
    user-select: none;
  }
  .segment:hover {
    background: var(--bg-hover);
    color: var(--fg-primary);
  }
  .segment.current {
    color: var(--fg-primary);
  }
  .sep {
    flex-shrink: 0;
    font-size: var(--text-small);
    color: var(--fg-secondary);
    opacity: 0.6;
  }
</style>
