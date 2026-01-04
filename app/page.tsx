import Link from 'next/link'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { 
  ShoppingBag, 
  MessageSquare, 
  Newspaper, 
  Users, 
  Gamepad2,
  TrendingUp 
} from 'lucide-react'

export default function HomePage() {
  return (
    <div className="container mx-auto px-4 py-8">
      {/* Hero Section */}
      <section className="text-center py-16 mb-16">
        <h1 className="text-5xl font-bold mb-4 bg-gradient-to-r from-rust-400 to-rust-600 bg-clip-text text-transparent">
          Welcome to Art of Rust
        </h1>
        <p className="text-xl text-gray-300 mb-8 max-w-2xl mx-auto">
          Your ultimate destination for Rust gaming community. Join thousands of players, 
          share strategies, shop for items, and stay updated with the latest news.
        </p>
        <div className="flex gap-4 justify-center">
          <Button asChild size="lg" className="bg-rust-600 hover:bg-rust-700">
            <Link href="/register">Get Started</Link>
          </Button>
          <Button asChild size="lg" variant="outline">
            <Link href="/forum">Visit Forum</Link>
          </Button>
        </div>
      </section>

      {/* Features Grid */}
      <section className="grid md:grid-cols-2 lg:grid-cols-3 gap-6 mb-16">
        <Card className="p-6 hover:border-rust-500 transition-colors">
          <ShoppingBag className="w-12 h-12 text-rust-500 mb-4" />
          <h3 className="text-xl font-semibold mb-2">Shop</h3>
          <p className="text-gray-400 mb-4">
            Browse our marketplace for game items, cosmetics, and exclusive content.
          </p>
          <Button asChild variant="ghost">
            <Link href="/shop">Visit Shop →</Link>
          </Button>
        </Card>

        <Card className="p-6 hover:border-rust-500 transition-colors">
          <MessageSquare className="w-12 h-12 text-rust-500 mb-4" />
          <h3 className="text-xl font-semibold mb-2">Forum</h3>
          <p className="text-gray-400 mb-4">
            Join discussions, share strategies, and connect with the community.
          </p>
          <Button asChild variant="ghost">
            <Link href="/forum">Visit Forum →</Link>
          </Button>
        </Card>

        <Card className="p-6 hover:border-rust-500 transition-colors">
          <Newspaper className="w-12 h-12 text-rust-500 mb-4" />
          <h3 className="text-xl font-semibold mb-2">News</h3>
          <p className="text-gray-400 mb-4">
            Stay updated with the latest game updates, patches, and community news.
          </p>
          <Button asChild variant="ghost">
            <Link href="/news">Read News →</Link>
          </Button>
        </Card>

        <Card className="p-6 hover:border-rust-500 transition-colors">
          <Users className="w-12 h-12 text-rust-500 mb-4" />
          <h3 className="text-xl font-semibold mb-2">Community</h3>
          <p className="text-gray-400 mb-4">
            Connect with players, join clans, and build lasting friendships.
          </p>
          <Button asChild variant="ghost">
            <Link href="/users">Browse Users →</Link>
          </Button>
        </Card>

        <Card className="p-6 hover:border-rust-500 transition-colors">
          <Gamepad2 className="w-12 h-12 text-rust-500 mb-4" />
          <h3 className="text-xl font-semibold mb-2">Game Stats</h3>
          <p className="text-gray-400 mb-4">
            Track your progress, view leaderboards, and compare with friends.
          </p>
          <Button asChild variant="ghost">
            <Link href="/stats">View Stats →</Link>
          </Button>
        </Card>

        <Card className="p-6 hover:border-rust-500 transition-colors">
          <TrendingUp className="w-12 h-12 text-rust-500 mb-4" />
          <h3 className="text-xl font-semibold mb-2">Server Status</h3>
          <p className="text-gray-400 mb-4">
            Check server status, player counts, and real-time information.
          </p>
          <Button asChild variant="ghost">
            <Link href="/servers">View Servers →</Link>
          </Button>
        </Card>
      </section>

      {/* Latest News Preview */}
      <section className="mb-16">
        <div className="flex justify-between items-center mb-6">
          <h2 className="text-3xl font-bold">Latest News</h2>
          <Button asChild variant="outline">
            <Link href="/news">View All</Link>
          </Button>
        </div>
        <div className="grid md:grid-cols-3 gap-6">
          {/* News items will be fetched from API */}
          <Card className="p-6">
            <div className="h-48 bg-gray-700 rounded mb-4"></div>
            <h3 className="text-lg font-semibold mb-2">Coming Soon</h3>
            <p className="text-gray-400 text-sm">News articles will appear here</p>
          </Card>
        </div>
      </section>
    </div>
  )
}

