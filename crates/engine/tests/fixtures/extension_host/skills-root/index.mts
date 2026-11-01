export const inject = ['skills']
export function apply(ctx: any) {
  ctx.skills.registerRoot({ path: 'profiles/review-skills' })
}
