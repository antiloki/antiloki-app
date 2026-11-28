// Not Flow-typed: the alias must resolve from plain JavaScript too.
import getComponentNameFromType from 'shared/getComponentNameFromType';

export function plain(type) {
  return getComponentNameFromType(type);
}
