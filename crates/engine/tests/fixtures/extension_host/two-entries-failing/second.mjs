// Second entry: fails activation.
export const name = 'two-entries-failing-second'

export function apply() {
  throw new Error('second entry refuses to start')
}
