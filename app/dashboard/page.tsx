import { getServerSession } from 'next-auth'
import { authOptions } from '@/lib/auth'
import { redirect } from 'next/navigation'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import Link from 'next/link'
import { prisma } from '@/lib/prisma'
import { ShoppingBag, MessageSquare, FileText, Settings } from 'lucide-react'

async function getUserStats(userId: string) {
  try {
    const [orders, forumPosts, comments] = await Promise.all([
      prisma.order.count({ where: { userId } }),
      prisma.forumPost.count({ where: { authorId: userId } }),
      prisma.comment.count({ where: { authorId: userId } }),
    ])

    return { orders, forumPosts, comments }
  } catch (error) {
    console.error('Error fetching user stats:', error)
    return { orders: 0, forumPosts: 0, comments: 0 }
  }
}

export default async function DashboardPage() {
  const session = await getServerSession(authOptions)

  if (!session) {
    redirect('/login')
  }

  const stats = await getUserStats(session.user.id)

  return (
    <div className="container mx-auto px-4 py-8">
      <div className="mb-8">
        <h1 className="text-4xl font-bold mb-2">Dashboard</h1>
        <p className="text-gray-400">Welcome back, {session.user.name || session.user.username}!</p>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-6 mb-8">
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <ShoppingBag className="w-5 h-5 text-rust-500" />
              Orders
            </CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-3xl font-bold">{stats.orders}</p>
            <p className="text-sm text-gray-400 mt-2">Total orders</p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <MessageSquare className="w-5 h-5 text-rust-500" />
              Forum Posts
            </CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-3xl font-bold">{stats.forumPosts}</p>
            <p className="text-sm text-gray-400 mt-2">Posts created</p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <FileText className="w-5 h-5 text-rust-500" />
              Comments
            </CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-3xl font-bold">{stats.comments}</p>
            <p className="text-sm text-gray-400 mt-2">Comments made</p>
          </CardContent>
        </Card>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
        <Card>
          <CardHeader>
            <CardTitle>Quick Actions</CardTitle>
            <CardDescription>Common tasks and shortcuts</CardDescription>
          </CardHeader>
          <CardContent className="space-y-2">
            <Button asChild variant="outline" className="w-full justify-start">
              <Link href="/shop">
                <ShoppingBag className="w-4 h-4 mr-2" />
                Browse Shop
              </Link>
            </Button>
            <Button asChild variant="outline" className="w-full justify-start">
              <Link href="/forum/new">
                <MessageSquare className="w-4 h-4 mr-2" />
                Create Forum Post
              </Link>
            </Button>
            <Button asChild variant="outline" className="w-full justify-start">
              <Link href="/dashboard/settings">
                <Settings className="w-4 h-4 mr-2" />
                Account Settings
              </Link>
            </Button>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Recent Activity</CardTitle>
            <CardDescription>Your latest actions</CardDescription>
          </CardHeader>
          <CardContent>
            <p className="text-gray-400 text-sm">No recent activity</p>
          </CardContent>
        </Card>
      </div>
    </div>
  )
}

