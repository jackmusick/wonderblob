<script lang="ts">
  import "../lib/styles/app.css";
  import { onMount } from "svelte";

  let { children } = $props();

  onMount(() => {
    const onContextMenu = (e: MouseEvent) => e.preventDefault();
    const onKeydown = (e: KeyboardEvent) => {
      // block browser zoom / find / print chords
      if ((e.ctrlKey || e.metaKey) && ["+", "-", "=", "0", "p", "f"].includes(e.key)) {
        e.preventDefault();
      }
    };
    if (!import.meta.env.DEV) {
      window.addEventListener("contextmenu", onContextMenu);
    }
    window.addEventListener("keydown", onKeydown);
    return () => {
      window.removeEventListener("contextmenu", onContextMenu);
      window.removeEventListener("keydown", onKeydown);
    };
  });
</script>

{@render children()}
