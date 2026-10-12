import { used, type Props, Button } from './components';
import * as utils from './utils';
import { icons } from './barrel';
import legacy from './legacy';
import './augment';
import { helped } from './helpers';

// An entry's re-exports are its interface, so they count as used.
export { fromEntry } from './re-exported';

utils.format();
console.log(icons.Star);
used();
Button({} as Props);
legacy();
helped;

const { lazyUsed } = await import('./lazy');
lazyUsed();
