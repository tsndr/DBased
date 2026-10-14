const VIEWS = {
  content: {
    badge: "Content View",
    title: "Spreadsheet Data Grid & Multiline JSONB Vim Inspector",
    desc: "Browse and filter rows at native speed, edit cells inline with transactional batch commits, jump across foreign keys (>), and inspect JSON/JSONB/XML/TEXT columns in a side-by-side GtkSourceView 5 Vim panel.",
    img: "assets/content.png",
    alt: "DBased Table Content View and Multiline JSONB Vim Editor",
    shortcuts: [
      { keys: ["Ctrl", "S"], label: "Commit batch edits" },
      { keys: [":w"], label: "Stage JSONB cell" },
      { keys: [":wq"], label: "Stage & close panel" },
      { keys: ["Alt", "←"], label: "FK jump back" },
    ],
  },
  query: {
    badge: "Query View",
    title: "Vim-Powered SQL Query Editor",
    desc: "Write ad-hoc SQL in a syntax-highlighted GtkSourceView 5 buffer with full NORMAL, INSERT, VISUAL, and command-line Vim modes. Execute the entire buffer or only your active visual selection with sub-millisecond timing telemetry.",
    img: "assets/query.png",
    alt: "DBased SQL Query Editor in Vim Mode",
    shortcuts: [
      { keys: ["Ctrl", "Enter"], label: "Run query / selection" },
      { keys: [":r"], label: "Execute SQL" },
      { keys: [":w"], label: "Save .sql script" },
      { keys: ["v"], label: "Visual selection" },
    ],
  },
  structure: {
    badge: "Structure View",
    title: "Schema Inspector & Drag-and-Drop Column Reordering",
    desc: "Inspect column types, nullability, primary keys (PRI), defaults, and extra attributes. Add, modify, or drop columns via guided dialogs, or drag rows to reorder columns natively on both MySQL/MariaDB and PostgreSQL.",
    img: "assets/structure.png",
    alt: "DBased Table Structure and Drag-and-Drop Column Reordering",
    shortcuts: [
      { keys: ["Drag Row"], label: "Reorder column position" },
      { keys: ["PostgreSQL"], label: "Transactional rebuild (FKs/views preserved)" },
      { keys: ["MySQL"], label: "ALTER TABLE ... MODIFY ... AFTER" },
    ],
  },
  relations: {
    badge: "Relations View",
    title: "Foreign Key Constraints & Cascading Rules Editor",
    desc: "Inspect foreign key constraints, local columns, target tables, and ON UPDATE / ON DELETE actions (NO ACTION, RESTRICT, CASCADE, SET NULL). Click any target table link to jump directly to the referenced schema.",
    img: "assets/relations.png",
    alt: "DBased Foreign Key Relations View",
    shortcuts: [
      { keys: ["Click Target"], label: "Jump to referenced table" },
      { keys: ["Add Relation"], label: "Create FK constraint" },
      { keys: ["CASCADE / RESTRICT"], label: "Referential rules" },
    ],
  },
  connection: {
    badge: "Connection Setup",
    title: "Bookmarks, URL Quick-Paste & Built-in SSH Bastion Tunneling",
    desc: "Manage saved connection profiles backed by your OS keyring (oo7 / Secret Service), paste postgres:// or mysql:// URLs to connect immediately, or forward traffic through encrypted SSH jump hosts via russh.",
    img: "assets/connection.png",
    alt: "DBased Connection Setup and SSH Bastion Tunneling",
    shortcuts: [
      { keys: ["postgres://"], label: "Quick URL paste" },
      { keys: ["SSH Key / Pass"], label: "Native russh bastion tunnel" },
      { keys: ["oo7 Keyring"], label: "D-Bus Secret Service" },
    ],
  },
};

function resolveImgWithFallback(imgEl, relativePath) {
  imgEl.onerror = function () {
    if (!imgEl.dataset.triedParent) {
      imgEl.dataset.triedParent = "1";
      imgEl.src = "../" + relativePath;
    } else if (!imgEl.dataset.triedRaw) {
      imgEl.dataset.triedRaw = "1";
      imgEl.src =
        "https://raw.githubusercontent.com/tsndr/dbased/main/" + relativePath;
    }
  };
  delete imgEl.dataset.triedParent;
  delete imgEl.dataset.triedRaw;
  imgEl.src = relativePath;
}

document.addEventListener("DOMContentLoaded", () => {
  // Preload screenshots for instant view switching
  Object.values(VIEWS).forEach((v) => {
    const preload = new Image();
    resolveImgWithFallback(preload, v.img);
  });

  // 1. Workspace View Switcher
  const tabBtns = document.querySelectorAll(".adw-tab-btn");
  const showcaseImg = document.getElementById("showcase-img");
  const captionBadge = document.getElementById("caption-badge");
  const captionTitle = document.getElementById("caption-title");
  const captionDesc = document.getElementById("caption-desc");
  const captionShortcuts = document.getElementById("caption-shortcuts");

  function renderShortcuts(shortcuts) {
    captionShortcuts.innerHTML = shortcuts
      .map((item) => {
        const keysHtml = item.keys
          .map((k) => `<kbd>${k}</kbd>`)
          .join("<span>+</span>");
        return `<span class="shortcut-chip">${keysHtml} <span>${item.label}</span></span>`;
      })
      .join("");
  }

  function selectView(viewKey) {
    const data = VIEWS[viewKey];
    if (!data) return;

    tabBtns.forEach((btn) => {
      const isMatch = btn.dataset.view === viewKey;
      btn.classList.toggle("active", isMatch);
      btn.setAttribute("aria-selected", isMatch ? "true" : "false");
    });

    showcaseImg.style.opacity = "0.35";
    setTimeout(() => {
      resolveImgWithFallback(showcaseImg, data.img);
      showcaseImg.alt = data.alt;
      showcaseImg.style.opacity = "1";
    }, 90);

    captionBadge.textContent = data.badge;
    captionTitle.textContent = data.title;
    captionDesc.textContent = data.desc;
    renderShortcuts(data.shortcuts);
  }

  tabBtns.forEach((btn) => {
    btn.addEventListener("click", () => {
      selectView(btn.dataset.view);
    });
  });

  // Number keys 1-5 switch workspace views
  const viewOrder = ["content", "query", "structure", "relations", "connection"];
  document.addEventListener("keydown", (e) => {
    if (
      e.target.tagName === "INPUT" ||
      e.target.tagName === "TEXTAREA" ||
      e.metaKey ||
      e.ctrlKey ||
      e.altKey
    ) {
      return;
    }
    const idx = parseInt(e.key, 10) - 1;
    if (idx >= 0 && idx < viewOrder.length) {
      selectView(viewOrder[idx]);
    }
  });

  // 2. Lightbox zoom for screenshots
  const lightbox = document.getElementById("lightbox");
  const lightboxImg = document.getElementById("lightbox-img");
  const lightboxClose = document.getElementById("lightbox-close");
  const showcaseFrame = document.getElementById("showcase-frame");

  if (showcaseFrame && lightbox && lightboxImg) {
    showcaseFrame.addEventListener("click", () => {
      lightboxImg.src = showcaseImg.src;
      lightboxImg.alt = showcaseImg.alt;
      lightbox.classList.add("open");
      lightbox.setAttribute("aria-hidden", "false");
    });

    const closeLightbox = () => {
      lightbox.classList.remove("open");
      lightbox.setAttribute("aria-hidden", "true");
    };

    lightboxClose.addEventListener("click", closeLightbox);
    lightbox.addEventListener("click", (e) => {
      if (e.target === lightbox) closeLightbox();
    });
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && lightbox.classList.contains("open")) {
        closeLightbox();
      }
    });
  }

  // 3. Distribution / Package Installer Tabs
  const distroTabs = document.querySelectorAll(".distro-tab");
  const distroPanels = document.querySelectorAll(".distro-panel");

  distroTabs.forEach((tab) => {
    tab.addEventListener("click", () => {
      const target = tab.dataset.distro;
      distroTabs.forEach((t) => {
        const active = t.dataset.distro === target;
        t.classList.toggle("active", active);
        t.setAttribute("aria-selected", active ? "true" : "false");
      });
      distroPanels.forEach((panel) => {
        panel.classList.toggle("active", panel.id === `distro-${target}`);
      });
    });
  });

  // 4. Copy to Clipboard buttons
  const copyButtons = document.querySelectorAll("[data-copy]");
  copyButtons.forEach((btn) => {
    btn.addEventListener("click", async () => {
      const text = btn.getAttribute("data-copy");
      if (!text) return;
      try {
        await navigator.clipboard.writeText(text);
        const labelEl = btn.querySelector(".copy-label");
        const originalText = labelEl ? labelEl.textContent : "";
        btn.classList.add("copied");
        if (labelEl) labelEl.textContent = "Copied!";
        setTimeout(() => {
          btn.classList.remove("copied");
          if (labelEl) labelEl.textContent = originalText;
        }, 1800);
      } catch (_) {
        // Fallback selection
      }
    });
  });
});
