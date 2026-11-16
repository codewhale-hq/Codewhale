export const name = 'dsh-attachments'
export const inject = ['tools', 'attachments']

export function apply(ctx) {
  ctx.tools.register({
    name: 'attachments_roundtrip',
    description: 'Validate, read, save, thumbnail-request, and emit an image attachment.',
    parameters: {
      type: 'object',
      properties: {
        sourceRef: {
          type: 'object',
          properties: {
            attachmentId: { type: 'string' },
            mediaType: { type: 'string' },
            bytes: { type: 'number' },
            width: { type: 'number' },
            height: { type: 'number' },
          },
          required: ['attachmentId'],
        },
        saveBase64: { type: 'string' },
        mediaType: { type: 'string' },
        imageName: { type: 'string' },
      },
      required: ['sourceRef', 'saveBase64', 'mediaType'],
      additionalProperties: false,
    },
    output: {
      schema: {
        type: 'object',
        properties: {
          attachment: {
            type: 'object',
            properties: {
              attachmentId: { type: 'string' },
              mediaType: { type: 'string' },
              bytes: { type: 'number' },
              width: { type: 'number' },
              height: { type: 'number' },
            },
            required: ['attachmentId', 'mediaType', 'bytes', 'width', 'height'],
          },
          summary: { type: 'string' },
        },
        required: ['attachment', 'summary'],
      },
      render: (_args, value) => [
        { type: 'text', text: value.summary },
        { type: 'image', attachment: value.attachment },
      ],
    },
    async execute(input, exec) {
      const limits = ctx.attachments.imageLimits
      const raw = Buffer.from(input.saveBase64, 'base64')
      if (!limits.mediaTypes.includes(input.mediaType) || raw.byteLength > limits.maxImageBytes) {
        throw new Error('exceeds imageLimits')
      }
      const readBack = await ctx.attachments.readImage(input.sourceRef, exec.signal)
      await ctx.attachments.validateImage({
        data: raw,
        mediaType: input.mediaType,
        ...(input.imageName === undefined ? {} : { name: input.imageName }),
      })
      const saved = await ctx.attachments.saveImage({
        data: raw,
        mediaType: input.mediaType,
        name: input.imageName ?? 'studio-image',
      })
      const thumb = await ctx.attachments.readImageRequest(
        saved,
        { width: 320, height: 320, maxBytes: 98304 },
        exec.signal,
      )
      return {
        attachment: saved,
        summary: `read=${readBack.data.byteLength} saved=${saved.attachmentId} thumb=${thumb.data.byteLength}`,
      }
    },
  })
}
