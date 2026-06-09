<script lang="ts" module>
  // Inline SVG icon set — Lucide-style line icons (24×24, stroke=currentColor),
  // no dependency. Each entry is the inner markup of the <svg>; elements that
  // must be solid set fill explicitly (the wrapper is fill:none).
  export const ICONS: Record<string, string> = {
    // generic file-system
    folder:
      '<path d="M3 7a2 2 0 0 1 2-2h3.6a1 1 0 0 1 .7.3L11 7h6a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
    "folder-plus":
      '<path d="M3 7a2 2 0 0 1 2-2h3.6a1 1 0 0 1 .7.3L11 7h6a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><path d="M12 11v5M9.5 13.5h5"/>',
    file: '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/>',
    "file-text":
      '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/><path d="M9 13h6M9 16.5h6"/>',
    "file-image":
      '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/><circle cx="9.5" cy="13" r="1.3"/><path d="M19 17l-3.5-3.5L10 19"/>',
    "file-code":
      '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/><path d="M10 12.5 8 14.5l2 2M14 12.5l2 2-2 2"/>',
    "file-archive":
      '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/><path d="M10.5 7v1.5M10.5 10.5v1.5M10.5 14v1.5"/>',
    "file-pdf":
      '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/><text x="7.2" y="18.5" font-size="5.5" fill="currentColor" stroke="none" font-weight="600">PDF</text>',
    // (protocol icons — sftp/s3/azure/onedrive — come from brand-icons.ts)
    // actions
    download: '<path d="M12 3v12"/><path d="M7 11l5 4 5-4"/><path d="M5 20h14"/>',
    upload: '<path d="M12 21V9"/><path d="M7 13l5-4 5 4"/><path d="M5 4h14"/>',
    share:
      '<circle cx="18" cy="5" r="3"/><circle cx="6" cy="12" r="3"/><circle cx="18" cy="19" r="3"/><path d="M8.6 13.5l6.8 4M15.4 6.5l-6.8 4"/>',
    power: '<path d="M12 4v8"/><path d="M8 6.3a7 7 0 1 0 8 0"/>',
    settings:
      '<path d="M4 7h9M17 7h3"/><circle cx="15" cy="7" r="2"/><path d="M4 17h3M11 17h9"/><circle cx="9" cy="17" r="2"/>',
    transfers:
      '<path d="M7 7v10M7 7l-3 3M7 7l3 3"/><path d="M17 17V7M17 17l-3-3M17 17l3-3"/>',
    "dots-vertical":
      '<circle cx="12" cy="5" r="1.4" fill="currentColor" stroke="none"/><circle cx="12" cy="12" r="1.4" fill="currentColor" stroke="none"/><circle cx="12" cy="19" r="1.4" fill="currentColor" stroke="none"/>',
    trash:
      '<path d="M4 7h16"/><path d="M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/><path d="M6 7l1 13a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1l1-13"/>',
    pencil: '<path d="M4 20h4L19.5 8.5a2 2 0 0 0-3-3L5 17z"/>',
    search: '<circle cx="11" cy="11" r="7"/><path d="M21 21l-4.3-4.3"/>',
    x: '<path d="M6 6l12 12M18 6 6 18"/>',
    check: '<path d="M5 12l5 5 9-11"/>',
    dot: '<circle cx="12" cy="12" r="4" fill="currentColor" stroke="none"/>',
    "chevron-right": '<path d="M9 6l6 6-6 6"/>',
    "chevron-down": '<path d="M6 9l6 6 6-6"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
  };
</script>

<script lang="ts">
  import { BRAND_ICONS } from "./brand-icons";

  let { name, size = 16 }: { name: keyof typeof ICONS | string; size?: number } =
    $props();

  // Brand glyphs (Simple Icons) are solid fills; the rest of the set is stroked.
  const brand = $derived(BRAND_ICONS[name]);
</script>

{#if brand}
  <svg
    class="icon"
    width={size}
    height={size}
    viewBox="0 0 24 24"
    fill="currentColor"
    stroke="none"
    aria-hidden="true"
  >
    <!-- eslint-disable-next-line svelte/no-at-html-tags -- static, trusted icon table -->
    {@html brand}
  </svg>
{:else}
  <svg
    class="icon"
    width={size}
    height={size}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <!-- eslint-disable-next-line svelte/no-at-html-tags -- static, trusted icon table -->
    {@html ICONS[name] ?? ICONS.file}
  </svg>
{/if}

<style>
  .icon {
    display: inline-block;
    flex: none;
    vertical-align: middle;
  }
</style>
