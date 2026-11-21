/**
 * Hide the diagram's own header so an embedded view does not repeat what the
 * app already draws around it. LikeC4 only honors the rule inside its shadow
 * root, and the navigation panel slots are a shortcut for 1.60: recheck the
 * logo and breadcrumb children when upgrading `@likec4/diagram`.
 */
export const HIDDEN_DIAGRAM_CHROME = `
  [data-panel="navigation"] > div:nth-child(-n + 2) {
    display: none;
  }
`;
