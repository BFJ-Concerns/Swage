/**
 * Props a test can change after `mount`: writes to the returned object reach
 * the mounted component as prop updates, as a parent's would.
 */
export function reactiveProps<T extends object>(initial: T): T {
  const props = $state(initial);
  return props;
}
