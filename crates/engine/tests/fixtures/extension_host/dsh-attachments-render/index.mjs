export const name = 'dsh-attachments-render'
export const inject = ['tools']

export function apply(ctx) {
  ctx.tools.register({
    name: 'render_attachment_result',
    description: 'Return either a rendered { type: image, attachment } block or plain text.',
    parameters: {
      type: 'object',
      properties: {
        attachmentId: { type: 'string' },
        textOnly: { type: 'string' },
      },
      additionalProperties: false,
    },
    output: {
      schema: {
        type: 'object',
        properties: {
          attachmentId: { type: 'string' },
          textOnly: { type: 'string' },
        },
      },
      render: (_args, value) => {
        if (typeof value.textOnly === 'string') {
          return [{ type: 'text', text: value.textOnly }]
        }
        return [
          { type: 'text', text: `image ${value.attachmentId}` },
          { type: 'image', attachment: { attachmentId: value.attachmentId } },
        ]
      },
    },
    execute: (input) => input,
  })
}
