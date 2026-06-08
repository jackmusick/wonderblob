<script lang="ts">
  import "../lib/styles/app.css";
  import { onMount } from "svelte";

  let { children } = $props();

  onMount(() => {
    if (!import.meta.env.DEV) {
      window.addEventListener("contextmenu", (e) => e.preventDefault());
    }
    window.addEventListener("keydown", (e) => {
      // block browser zoom / find / print chords
      if ((e.ctrlKey || e.metaKey) && ["+", "-", "=", "0", "p", "f"].includes(e.key)) {
        e.preventDefault();
      }
    });
  });
</script>

{@render children()}
