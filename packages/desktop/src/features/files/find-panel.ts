import type { EditorView, Panel, ViewUpdate } from "@uiw/react-codemirror";
import {
  SearchQuery,
  closeSearchPanel,
  findNext,
  findPrevious,
  getSearchQuery,
  replaceAll,
  replaceNext,
  selectMatches,
  setSearchQuery,
} from "@codemirror/search";

const ICONS = {
  chevron: '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  up: '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 12.5v-9M4.5 7 8 3.5 11.5 7" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  down: '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 3.5v9M4.5 9 8 12.5 11.5 9" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  all: '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 4.5h10M3 8h10M3 11.5h10" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/></svg>',
  close: '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/></svg>',
};

function button(className: string, label: string, title: string, icon: string) {
  const element = document.createElement("button");
  element.type = "button";
  element.className = className;
  element.setAttribute("aria-label", label);
  element.title = title;
  element.innerHTML = icon;
  // Keep the caret in the field while stepping through matches.
  element.addEventListener("mousedown", (event) => event.preventDefault());
  return element;
}

function field(className: string) {
  const element = document.createElement("div");
  element.className = `file-find-field${className ? ` ${className}` : ""}`;
  return element;
}

function input(label: string, placeholder: string) {
  const element = document.createElement("input");
  element.className = "file-find-input";
  element.type = "text";
  element.setAttribute("aria-label", label);
  element.placeholder = placeholder;
  element.autocomplete = "off";
  element.spellcheck = false;
  return element;
}

/** Command+F panel for the file editor, styled after VS Code's find widget. */
export class FileFindPanel implements Panel {
  dom: HTMLElement;
  pos = 80;

  private view: EditorView;
  private query: SearchQuery;
  private expanded = false;
  private searchInput: HTMLInputElement;
  private replaceInput: HTMLInputElement;
  private count: HTMLElement;
  private twist: HTMLButtonElement;
  private replaceRow: HTMLElement;
  private readOnlySpacer: HTMLElement;
  private readonly options: Record<"case" | "word" | "regexp", HTMLButtonElement>;

  constructor(view: EditorView) {
    this.view = view;
    this.query = getSearchQuery(view.state);
    this.dom = document.createElement("div");
    this.dom.className = "file-find";
    this.dom.setAttribute("role", "search");
    this.dom.setAttribute("aria-label", "Find in file");
    this.dom.addEventListener("keydown", (event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        closeSearchPanel(this.view);
      } else if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "f") {
        event.preventDefault();
        this.searchInput.focus();
        this.searchInput.select();
      }
    });

    this.twist = button("file-find-twist", "Toggle replace", "Toggle replace", ICONS.chevron);
    this.twist.setAttribute("aria-expanded", "false");
    this.twist.addEventListener("click", () => this.toggleReplace());

    this.readOnlySpacer = document.createElement("span");
    this.readOnlySpacer.className = "file-find-spacer";
    this.readOnlySpacer.setAttribute("aria-hidden", "true");

    this.searchInput = input("Find", "Find");
    this.searchInput.addEventListener("input", () => this.commit());
    this.searchInput.addEventListener("keydown", (event) => this.searchKeydown(event));

    this.options = {
      case: this.option("Aa", "Match case", "Match case (Alt+C)"),
      word: this.option("ab", "Match whole word", "Match whole word (Alt+W)"),
      regexp: this.option(".*", "Use regular expression", "Use regular expression (Alt+R)"),
    };
    this.options.word.classList.add("file-find-word");
    this.options.case.addEventListener("click", () => this.patch({ caseSensitive: !this.query.caseSensitive }));
    this.options.word.addEventListener("click", () => this.patch({ wholeWord: !this.query.wholeWord }));
    this.options.regexp.addEventListener("click", () => this.patch({ regexp: !this.query.regexp }));

    const optionBar = document.createElement("div");
    optionBar.className = "file-find-options";
    optionBar.append(this.options.case, this.options.word, this.options.regexp);

    const searchField = field("");
    searchField.append(this.searchInput, optionBar);

    this.count = document.createElement("span");
    this.count.className = "file-find-count";
    this.count.setAttribute("aria-live", "polite");

    const previous = button("file-find-action", "Previous match", "Previous match (Shift+Enter)", ICONS.up);
    previous.addEventListener("click", () => this.navigate(findPrevious));
    const next = button("file-find-action", "Next match", "Next match (Enter)", ICONS.down);
    next.addEventListener("click", () => this.navigate(findNext));
    const all = button("file-find-action", "Select all matches", "Select all matches", ICONS.all);
    all.addEventListener("click", () => {
      selectMatches(this.view);
      this.refresh();
    });
    const close = button("file-find-action file-find-close", "Close find", "Close find (Esc)", ICONS.close);
    close.addEventListener("click", () => closeSearchPanel(this.view));

    const main = document.createElement("div");
    main.className = "file-find-row";
    main.append(this.twist, this.readOnlySpacer, searchField, this.count, previous, next, all, close);

    this.replaceInput = input("Replace", "Replace");
    this.replaceInput.addEventListener("input", () => this.commit());
    this.replaceInput.addEventListener("keydown", (event) => this.replaceKeydown(event));
    const replaceField = field("file-find-replace-field");
    replaceField.append(this.replaceInput);

    const replace = document.createElement("button");
    replace.type = "button";
    replace.className = "file-find-button";
    replace.textContent = "Replace";
    replace.addEventListener("mousedown", (event) => event.preventDefault());
    replace.addEventListener("click", () => this.replace(replaceNext));
    const replaceEverything = document.createElement("button");
    replaceEverything.type = "button";
    replaceEverything.className = "file-find-button";
    replaceEverything.textContent = "Replace All";
    replaceEverything.addEventListener("mousedown", (event) => event.preventDefault());
    replaceEverything.addEventListener("click", () => this.replace(replaceAll));

    const replaceRowSpacer = document.createElement("span");
    replaceRowSpacer.className = "file-find-spacer";
    replaceRowSpacer.setAttribute("aria-hidden", "true");

    this.replaceRow = document.createElement("div");
    this.replaceRow.className = "file-find-row file-find-replace";
    this.replaceRow.hidden = true;
    this.replaceRow.append(replaceRowSpacer, replaceField, replace, replaceEverything);

    this.dom.append(main, this.replaceRow);
    this.sync();
    this.syncReadOnly();
    this.refresh();
  }

  mount() {
    this.searchInput.focus();
    this.searchInput.select();
  }

  update(update: ViewUpdate) {
    const query = getSearchQuery(update.state);
    if (!query.eq(this.query)) {
      this.query = query;
      this.sync();
      this.refresh();
      return;
    }
    this.syncReadOnly();
    if (update.docChanged) this.refresh();
  }

  private option(text: string, label: string, title: string) {
    const element = document.createElement("button");
    element.type = "button";
    element.className = "file-find-option";
    element.textContent = text;
    element.setAttribute("aria-label", label);
    element.setAttribute("aria-pressed", "false");
    element.title = title;
    element.addEventListener("mousedown", (event) => event.preventDefault());
    return element;
  }

  private patch(config: {
    search?: string;
    replace?: string;
    caseSensitive?: boolean;
    regexp?: boolean;
    wholeWord?: boolean;
  }) {
    const next = new SearchQuery({
      search: config.search ?? this.searchInput.value,
      replace: config.replace ?? this.replaceInput.value,
      caseSensitive: config.caseSensitive ?? this.query.caseSensitive,
      regexp: config.regexp ?? this.query.regexp,
      wholeWord: config.wholeWord ?? this.query.wholeWord,
      literal: this.query.literal,
    });
    if (next.eq(this.query)) return;
    this.query = next;
    this.view.dispatch({ effects: setSearchQuery.of(next) });
    this.refresh();
  }

  private commit() {
    this.patch({});
  }

  private sync() {
    this.searchInput.value = this.query.search;
    this.replaceInput.value = this.query.replace;
    this.options.case.setAttribute("aria-pressed", String(this.query.caseSensitive));
    this.options.word.setAttribute("aria-pressed", String(this.query.wholeWord));
    this.options.regexp.setAttribute("aria-pressed", String(this.query.regexp));
  }

  private syncReadOnly() {
    const readOnly = this.view.state.readOnly;
    this.twist.hidden = readOnly;
    this.readOnlySpacer.hidden = !readOnly;
    this.replaceRow.hidden = readOnly || !this.expanded;
  }

  private refresh() {
    const query = this.query;
    if (!query.search) {
      this.setCount("", "");
      return;
    }
    if (!query.valid) {
      this.setCount("Invalid regex", "error");
      return;
    }
    let count = 0;
    let index = 0;
    const selection = this.view.state.selection.main;
    try {
      const cursor = query.getCursor(this.view.state);
      for (let result = cursor.next(); !result.done; result = cursor.next()) {
        const match = result.value;
        count++;
        if (match.from === selection.from && match.to === selection.to) index = count;
      }
    } catch {
      this.setCount("Invalid regex", "error");
      return;
    }
    if (count === 0) this.setCount("No results", "empty");
    else if (index > 0) this.setCount(`${index} of ${count}`, "found");
    else this.setCount(`${count} result${count === 1 ? "" : "s"}`, "found");
  }

  private setCount(text: string, state: "" | "empty" | "error" | "found") {
    this.count.textContent = text;
    this.count.dataset.state = state;
  }

  private navigate(command: typeof findNext) {
    command(this.view);
    this.refresh();
  }

  private replace(command: typeof replaceNext) {
    command(this.view);
    this.refresh();
  }

  private toggleReplace() {
    this.expanded = !this.expanded;
    this.twist.setAttribute("aria-expanded", String(this.expanded));
    this.syncReadOnly();
    if (this.expanded) this.replaceInput.focus();
    else this.searchInput.focus();
  }

  private searchKeydown(event: KeyboardEvent) {
    if (event.key === "Enter") {
      event.preventDefault();
      this.navigate(event.shiftKey ? findPrevious : findNext);
    } else if (event.altKey && !event.metaKey && !event.ctrlKey) {
      const key = event.key.toLowerCase();
      if (key === "c") this.options.case.click();
      else if (key === "w") this.options.word.click();
      else if (key === "r") this.options.regexp.click();
    }
  }

  private replaceKeydown(event: KeyboardEvent) {
    if (event.key === "Enter") {
      event.preventDefault();
      this.replace(replaceNext);
    }
  }
}
