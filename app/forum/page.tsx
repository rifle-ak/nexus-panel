import { prisma } from '@/lib/prisma'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import Link from 'next/link'
import { formatDate } from '@/lib/utils'
import { MessageSquare, Lock, Pin } from 'lucide-react'

async function getCategories() {
  try {
    return await prisma.forumCategory.findMany({
      include: {
        posts: {
          take: 1,
          orderBy: { createdAt: 'desc' },
          include: {
            author: {
              select: {
                username: true,
                avatar: true,
              },
            },
          },
        },
        _count: {
          select: {
            posts: true,
          },
        },
      },
      orderBy: { order: 'asc' },
    })
  } catch (error) {
    console.error('Error fetching categories:', error)
    return []
  }
}

export default async function ForumPage() {
  const categories = await getCategories()

  return (
    <div className="container mx-auto px-4 py-8">
      <div className="mb-8 flex justify-between items-center">
        <div>
          <h1 className="text-4xl font-bold mb-2">Forum</h1>
          <p className="text-gray-400">Join the discussion and connect with the community</p>
        </div>
        <Button asChild>
          <Link href="/forum/new">New Post</Link>
        </Button>
      </div>

      {categories.length === 0 ? (
        <Card className="p-12 text-center">
          <p className="text-gray-400">No forum categories available yet.</p>
        </Card>
      ) : (
        <div className="space-y-4">
          {categories.map((category) => {
            const latestPost = category.posts[0]
            return (
              <Card key={category.id} className="hover:border-rust-500 transition-colors">
                <CardHeader>
                  <div className="flex items-start justify-between">
                    <div className="flex-1">
                      <CardTitle className="flex items-center gap-2">
                        <MessageSquare className="w-5 h-5 text-rust-500" />
                        <Link href={`/forum/category/${category.slug}`}>
                          {category.name}
                        </Link>
                      </CardTitle>
                      {category.description && (
                        <CardDescription className="mt-2">
                          {category.description}
                        </CardDescription>
                      )}
                    </div>
                    <div className="text-right">
                      <p className="text-sm font-semibold">{category._count.posts}</p>
                      <p className="text-xs text-gray-400">Posts</p>
                    </div>
                  </div>
                </CardHeader>
                {latestPost && (
                  <CardContent>
                    <div className="flex items-center justify-between text-sm">
                      <div className="flex items-center gap-2">
                        {latestPost.pinned && <Pin className="w-4 h-4 text-rust-500" />}
                        {latestPost.locked && <Lock className="w-4 h-4 text-gray-500" />}
                        <Link
                          href={`/forum/post/${latestPost.id}`}
                          className="hover:text-rust-400"
                        >
                          {latestPost.title}
                        </Link>
                      </div>
                      <div className="text-gray-400">
                        by {latestPost.author.username} • {formatDate(latestPost.createdAt)}
                      </div>
                    </div>
                  </CardContent>
                )}
              </Card>
            )
          })}
        </div>
      )}
    </div>
  )
}

