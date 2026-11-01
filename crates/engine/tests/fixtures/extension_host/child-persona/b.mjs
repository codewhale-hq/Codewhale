export const inject = ['prompt'];
export function apply(ctx) {
  ctx.prompt.registerSection({id:'persona-prefix', text:'Persona B model={{model}} cwd={{cwd}}', interpolate:'model-cwd'});
}
