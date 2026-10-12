import { external } from './elsewhere';

const local = 1;
// A renamed specifier is a deliberate rename, as in knip.
export { local as renamed, local };
export const copy = local;
// Passing on an import is a re-export, so `again` repeats nothing, as in knip.
export { external };
export const again = external;
