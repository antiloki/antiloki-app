/**
 * @flow
 */

import type {ReactContext} from 'shared/ReactTypes';
import getComponentNameFromType from 'shared/getComponentNameFromType';
import {reconcileChildFibers} from 'react-reconciler/src/ReactChildFiber';
import {Fragment} from 'react-reconciler';

// An opaque id and an exact object type: both Flow-only.
opaque type TagId = number;
export type Described = {|+name: ?string, +tag: TagId|};

export function describe(type: mixed): Described {
  const name: ?string = getComponentNameFromType((type: any));
  return {name, tag: (Fragment: any)};
}

export function reconcile(fiber: any) {
  return reconcileChildFibers(fiber);
}
