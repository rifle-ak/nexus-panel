import { prisma } from '@/lib/prisma'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import Link from 'next/link'
import { formatDate, truncate } from '@/lib/utils'
import Image from 'next/image'

async function getPosts() {
  try {
    return await prisma.post.findMany({
      where: { published: true },
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
      take: 12,
    })
  } catch (error) {
    console.error('Error fetching posts:', error)
    return []
  }
}

export default async function NewsPage() {
  const posts = await getPosts()

  return (
    <div className="container mx-auto px-4 py-8">
      <div className="mb-8">
        <h1 className="text-4xl font-bold mb-2">News & Updates</h1>
        <p className="text-gray-400">Stay updated with the latest from Art of Rust</p>
      </div>

      {posts.length === 0 ? (
        <Card className="p-12 text-center">
          <p className="text-gray-400">No news articles available yet.</p>
        </Card>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-6">
          {posts.map((post) => (
            <Card key={post.id} className="overflow-hidden hover:border-rust-500 transition-colors">
              {post.image && (
                <div className="relative h-48 w-full bg-gray-700">
                  <Image
                    src={post.image}
                    alt={post.title}
                    fill
                    className="object-cover"
                  />
                  {post.featured && (
                    <div className="absolute top-2 right-2 bg-rust-600 px-2 py-1 rounded text-xs font-semibold">
                      Featured
                    </div>
                  )}
                </div>
              )}
              <CardHeader>
                <div className="flex items-center gap-2 text-sm text-gray-400 mb-2">
                  <span>{post.author.username}</span>
                  <span>•</span>
                  <span>{formatDate(post.publishedAt || post.createdAt)}</span>
                </div>
                <CardTitle>
                  <Link href={`/news/${post.slug}`} className="hover:text-rust-400">
                    {post.title}
                  </Link>
                </CardTitle>
                {post.excerpt && (
                  <CardDescription>
                    {truncate(post.excerpt, 150)}
                  </CardDescription>
                )}
              </CardHeader>
              <CardContent>
                {post.tags.length > 0 && (
                  <div className="flex flex-wrap gap-2 mb-4">
                    {post.tags.map((tag) => (
                      <span
                        key={tag.id}
                        className="px-2 py-1 bg-gray-700 rounded text-xs"
                      >
                        {tag.name}
                      </span>
                    ))}
                  </div>
                )}
                <Button asChild variant="outline" className="w-full">
                  <Link href={`/news/${post.slug}`}>Read More</Link>
                </Button>
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </div>
  )
}

