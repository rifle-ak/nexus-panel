import { NextResponse } from 'next/server'
import { prisma } from '@/lib/prisma'
import { getServerSession } from 'next-auth'
import { authOptions } from '@/lib/auth'
import { z } from 'zod'
import { slugify } from '@/lib/utils'

const postSchema = z.object({
  title: z.string().min(1),
  content: z.string(),
  excerpt: z.string().optional(),
  published: z.boolean().default(false),
  featured: z.boolean().default(false),
  image: z.string().url().optional(),
  tags: z.array(z.string()).optional(),
})

export async function GET(request: Request) {
  try {
    const { searchParams } = new URL(request.url)
    const published = searchParams.get('published')
    const featured = searchParams.get('featured')
    const limit = parseInt(searchParams.get('limit') || '10')

    const posts = await prisma.post.findMany({
      where: {
        ...(published !== null && { published: published === 'true' }),
        ...(featured !== null && { featured: featured === 'true' }),
      },
      include: {
        author: {
          select: {
            username: true,
            avatar: true,
          },
        },
        tags: true,
      },
      orderBy: { publishedAt: 'desc' },
      take: limit,
    })

    return NextResponse.json(posts)
  } catch (error) {
    console.error('Error fetching posts:', error)
    return NextResponse.json(
      { error: 'Failed to fetch posts' },
      { status: 500 }
    )
  }
}

export async function POST(request: Request) {
  try {
    const session = await getServerSession(authOptions)
    if (!session) {
      return NextResponse.json(
        { error: 'Unauthorized' },
        { status: 401 }
      )
    }

    const body = await request.json()
    const validatedData = postSchema.parse(body)

    const slug = slugify(validatedData.title)
    
    // Ensure unique slug
    const existingPost = await prisma.post.findUnique({
      where: { slug },
    })

    let finalSlug = slug
    if (existingPost) {
      finalSlug = `${slug}-${Date.now()}`
    }

    const post = await prisma.post.create({
      data: {
        ...validatedData,
        slug: finalSlug,
        authorId: session.user.id,
        publishedAt: validatedData.published ? new Date() : null,
        tags: validatedData.tags
          ? {
              create: validatedData.tags.map((name) => ({ name })),
            }
          : undefined,
      },
      include: {
        author: true,
        tags: true,
      },
    })

    return NextResponse.json(post, { status: 201 })
  } catch (error) {
    if (error instanceof z.ZodError) {
      return NextResponse.json(
        { error: 'Invalid input', details: error.errors },
        { status: 400 }
      )
    }

    console.error('Error creating post:', error)
    return NextResponse.json(
      { error: 'Failed to create post' },
      { status: 500 }
    )
  }
}

