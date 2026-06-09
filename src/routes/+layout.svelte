<script lang="ts">
  import "../lib/styles/app.css";
  import { onMount } from "svelte";
  import { applyTheme, prefs } from "$lib/stores/prefs";

  let { children } = $props();

  // Keep the document's data-theme in sync with the theme preference.
  $effect(() => {
    applyTheme($prefs.theme);
  });

  onMount(() => {
    const onContextMenu = (e: MouseEvent) => e.preventDefault();
    const onKeydown = (e: KeyboardEvent) => {
      // block browser zoom / find / print chords
      if ((e.ctrlKey || e.metaKey) && ["+", "-", "=", "0", "p", "f"].includes(e.key)) {
        e.preventDefault();
      }
    };
    // Suppress the native webview menu (Reload / View Source / Inspect) app-wide
    // for native-app immersion. Components with their own right-click menus call
    // preventDefault locally, so this only blocks the default where nothing else
    // handled it. Applies in dev too — use F12 / Ctrl+Shift+I to open devtools.
    window.addEventListener("contextmenu", onContextMenu);
    window.addEventListener("keydown", onKeydown);
    return () => {
      window.removeEventListener("contextmenu", onContextMenu);
      window.removeEventListener("keydown", onKeydown);
    };
  });
</script>

{@render children()}
